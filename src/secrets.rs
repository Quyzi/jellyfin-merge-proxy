//! Secret encryption and key management.
//!
//! [`SecretCipher`] encrypts/decrypts secrets (upstream passwords and API keys)
//! with authenticated encryption. [`KeyStore`] manages the encryption key from
//! an env var or file. Encrypted secrets are prefixed (`enc:v1:<base64>`) and
//! plaintext is still accepted as input.

use crate::error::ConfigError;
use chacha20poly1305::{
    aead::{Aead, KeyInit, OsRng},
    XChaCha20Poly1305,
};
use rand::RngCore;
use std::fmt;
use std::path::Path;
use zeroize::Zeroize;

const ENC_PREFIX: &str = "enc:v1:";

// ---------------------------------------------------------------------------
// Key management
// ---------------------------------------------------------------------------

/// A 256-bit encryption key. Debug redacts; zeroized on drop.
#[derive(Clone)]
pub struct EncryptionKey([u8; 32]);

impl Drop for EncryptionKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl EncryptionKey {
    pub fn generate() -> Self {
        let mut key = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut key);
        EncryptionKey(key)
    }

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        EncryptionKey(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Load from a hex-encoded string (64 hex characters).
    pub fn from_hex(s: &str) -> Result<Self, ConfigError> {
        let bytes = hex::decode(s).map_err(|e| {
            ConfigError::Invalid(format!("encryption key must be 64 hex characters: {e}"))
        })?;
        if bytes.len() != 32 {
            return Err(ConfigError::Invalid(format!(
                "encryption key must be 32 bytes (64 hex), got {}",
                bytes.len()
            )));
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&bytes);
        Ok(EncryptionKey(key))
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Debug for EncryptionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EncryptionKey(***)")
    }
}

/// Where the encryption key comes from: env var, file, or auto-generated.
pub trait KeyStore: Send + Sync {
    /// Load or generate the key. `data_dir` is where an auto-generated key is saved.
    fn load(&self, data_dir: &Path) -> Result<EncryptionKey, ConfigError>;
    /// Human-readable description of the key source.
    fn describe(&self) -> String;
}

/// Default key management: JMP_SECRET_KEY env var, or `/var/lib/jmp/secret.key` file,
/// or auto-generated and saved to that file.
#[derive(Debug, Default)]
pub struct DefaultKeyStore;

impl KeyStore for DefaultKeyStore {
    fn load(&self, data_dir: &Path) -> Result<EncryptionKey, ConfigError> {
        // 1. Try env var first
        if let Ok(hex) = std::env::var("JMP_SECRET_KEY") {
            tracing::info!("loading encryption key from JMP_SECRET_KEY env var");
            return EncryptionKey::from_hex(&hex);
        }

        // 2. Try file
        let key_path = data_dir.join("secret.key");
        if key_path.exists() {
            tracing::info!(path = %key_path.display(), "loading encryption key from file");

            // Check file permissions on Unix
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let metadata = std::fs::metadata(&key_path).map_err(|e| ConfigError::Io {
                    path: key_path.clone(),
                    source: e,
                })?;
                let mode = metadata.permissions().mode();
                let group_read = (mode & 0o040) != 0;
                let other_read = (mode & 0o004) != 0;

                if group_read || other_read {
                    tracing::warn!(
                        path = %key_path.display(),
                        mode = format!("{:o}", mode & 0o777),
                        "encryption key file has insecure permissions (group or world readable). \
                         Recommended: chmod 600 {:?}",
                        key_path
                    );
                }
            }

            let hex = std::fs::read_to_string(&key_path).map_err(|e| ConfigError::Io {
                path: key_path.clone(),
                source: e,
            })?;
            return EncryptionKey::from_hex(hex.trim());
        }

        // 3. Auto-generate and save
        tracing::warn!(
            path = %key_path.display(),
            "no encryption key found; auto-generating and saving to file. BACK UP THIS FILE: \
             losing it means re-entering all upstream passwords and API keys."
        );
        let key = EncryptionKey::generate();
        std::fs::create_dir_all(data_dir).map_err(|e| ConfigError::Io {
            path: data_dir.to_path_buf(),
            source: e,
        })?;

        // Write with 0600 permissions
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&key_path, key.to_hex()).map_err(|e| ConfigError::Io {
                path: key_path.clone(),
                source: e,
            })?;
            let mut perms = std::fs::metadata(&key_path)
                .map_err(|e| ConfigError::Io {
                    path: key_path.clone(),
                    source: e,
                })?
                .permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(&key_path, perms).map_err(|e| ConfigError::Io {
                path: key_path.clone(),
                source: e,
            })?;
        }

        #[cfg(not(unix))]
        std::fs::write(&key_path, key.to_hex()).map_err(|e| ConfigError::Io {
            path: key_path.clone(),
            source: e,
        })?;

        tracing::info!(path = %key_path.display(), "saved auto-generated encryption key");
        Ok(key)
    }

    fn describe(&self) -> String {
        "JMP_SECRET_KEY env var or /var/lib/jmp/secret.key".into()
    }
}

// ---------------------------------------------------------------------------
// Encryption
// ---------------------------------------------------------------------------

/// Encrypts and decrypts secrets with authenticated encryption.
pub trait SecretCipher: Send + Sync {
    /// Encrypt a plaintext secret. Returns `enc:v1:<base64>`.
    fn encrypt(&self, plaintext: &str) -> Result<String, ConfigError>;
    /// Decrypt an `enc:v1:...` value. Returns an error if the prefix is missing
    /// or authentication fails.
    fn decrypt(&self, ciphertext: &str) -> Result<String, ConfigError>;
    /// True if the value is already encrypted.
    fn is_encrypted(&self, s: &str) -> bool {
        s.starts_with(ENC_PREFIX)
    }
}

/// XChaCha20-Poly1305 AEAD cipher.
pub struct XChaCha20Cipher {
    key: EncryptionKey,
}

impl XChaCha20Cipher {
    pub fn new(key: EncryptionKey) -> Self {
        XChaCha20Cipher { key }
    }
}

impl SecretCipher for XChaCha20Cipher {
    fn encrypt(&self, plaintext: &str) -> Result<String, ConfigError> {
        let cipher = XChaCha20Poly1305::new(self.key.as_bytes().into());
        let mut nonce_bytes = [0u8; 24];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = chacha20poly1305::XNonce::from(nonce_bytes);

        let mut ciphertext = cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|e| ConfigError::Invalid(format!("encryption failed: {e}")))?;

        // Prepend nonce to ciphertext
        let mut combined = nonce_bytes.to_vec();
        combined.append(&mut ciphertext);

        Ok(format!("{}{}", ENC_PREFIX, base64::encode(&combined)))
    }

    fn decrypt(&self, encrypted: &str) -> Result<String, ConfigError> {
        let encoded = encrypted.strip_prefix(ENC_PREFIX).ok_or_else(|| {
            ConfigError::Invalid("encrypted secret must start with enc:v1:".into())
        })?;

        let combined = base64::decode(encoded).map_err(|e| {
            ConfigError::Invalid(format!("invalid base64 in encrypted secret: {e}"))
        })?;

        if combined.len() < 24 {
            return Err(ConfigError::Invalid("encrypted secret too short".into()));
        }

        let (nonce_bytes, ciphertext) = combined.split_at(24);
        let nonce = chacha20poly1305::XNonce::from_slice(nonce_bytes);

        let cipher = XChaCha20Poly1305::new(self.key.as_bytes().into());
        let mut plaintext = cipher.decrypt(nonce, ciphertext).map_err(|_| {
            ConfigError::Invalid("decryption failed: wrong key or corrupted data".into())
        })?;

        let s = String::from_utf8(plaintext.clone()).map_err(|e| {
            plaintext.zeroize();
            ConfigError::Invalid(format!("decrypted secret is not valid UTF-8: {e}"))
        })?;
        plaintext.zeroize();
        Ok(s)
    }
}

// Use base64 from the standard base64 crate (already a dependency via other crates)
mod base64 {
    use base64::{engine::general_purpose::STANDARD, Engine};

    pub fn encode(bytes: &[u8]) -> String {
        STANDARD.encode(bytes)
    }

    pub fn decode(s: &str) -> Result<Vec<u8>, base64::DecodeError> {
        STANDARD.decode(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn key_round_trip() {
        let k = EncryptionKey::generate();
        let hex = k.to_hex();
        assert_eq!(hex.len(), 64);
        let k2 = EncryptionKey::from_hex(&hex).unwrap();
        assert_eq!(k.as_bytes(), k2.as_bytes());
    }

    #[test]
    fn key_debug_redacts() {
        let k = EncryptionKey::generate();
        let dbg = format!("{:?}", k);
        assert!(dbg.contains("***"));
        assert!(!dbg.contains(&k.to_hex()));
    }

    #[test]
    fn default_key_store_auto_generates() {
        let dir = tempdir().unwrap();
        let ks = DefaultKeyStore;
        let k = ks.load(dir.path()).unwrap();
        assert_eq!(k.as_bytes().len(), 32);
        let key_file = dir.path().join("secret.key");
        assert!(key_file.exists());
        let hex = std::fs::read_to_string(&key_file).unwrap();
        assert_eq!(hex.trim().len(), 64);
        // Second load reuses the file
        let k2 = ks.load(dir.path()).unwrap();
        assert_eq!(k.as_bytes(), k2.as_bytes());
    }

    #[test]
    fn default_key_store_env_var_overrides() {
        let dir = tempdir().unwrap();
        let ks = DefaultKeyStore;
        let key1 = EncryptionKey::generate();
        std::env::set_var("JMP_SECRET_KEY", key1.to_hex());
        let k = ks.load(dir.path()).unwrap();
        assert_eq!(k.as_bytes(), key1.as_bytes());
        // File is not created when env var is present
        assert!(!dir.path().join("secret.key").exists());
        std::env::remove_var("JMP_SECRET_KEY");
    }

    #[test]
    fn xchacha20_round_trip() {
        let key = EncryptionKey::generate();
        let cipher = XChaCha20Cipher::new(key);
        let plaintext = "hunter2";
        let encrypted = cipher.encrypt(plaintext).unwrap();
        assert!(encrypted.starts_with("enc:v1:"));
        assert!(!encrypted.contains("hunter2"));
        let decrypted = cipher.decrypt(&encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn xchacha20_wrong_key_fails() {
        let key1 = EncryptionKey::generate();
        let key2 = EncryptionKey::generate();
        let cipher1 = XChaCha20Cipher::new(key1);
        let cipher2 = XChaCha20Cipher::new(key2);
        let encrypted = cipher1.encrypt("secret").unwrap();
        assert!(cipher2.decrypt(&encrypted).is_err());
    }

    #[test]
    fn is_encrypted_check() {
        let key = EncryptionKey::generate();
        let cipher = XChaCha20Cipher::new(key);
        assert!(!cipher.is_encrypted("plaintext"));
        let enc = cipher.encrypt("plaintext").unwrap();
        assert!(cipher.is_encrypted(&enc));
    }
}
