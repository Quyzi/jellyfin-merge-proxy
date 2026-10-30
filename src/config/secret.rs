//! Secret values: upstream credentials ([`Secret`]) and raw user passwords.

use super::*;

// ---------------------------------------------------------------------------
// Secrets
// ---------------------------------------------------------------------------

/// A secret string (upstream password / API key). Debug/Display redact.
#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Secret(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    pub fn is_encrypted(&self) -> bool {
        self.0.starts_with("enc:v1:")
    }
    /// Decrypt if encrypted, otherwise return plaintext.
    pub fn decrypt(
        &self,
        cipher: &dyn crate::secrets::SecretCipher,
    ) -> Result<Secret, ConfigError> {
        if self.is_encrypted() {
            Ok(Secret(cipher.decrypt(&self.0)?))
        } else {
            Ok(self.clone())
        }
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}
impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Raw password as written in the TOML file.
/// Accepted forms: `"plain"`, `{ plain = "..." }`, `{ plaintext = "..." }`,
/// `{ argon2 = "$argon2id$..." }`, or a bare `"$argon2..."` PHC string.
#[derive(Clone, Deserialize)]
#[serde(untagged)]
pub enum RawPassword {
    Tagged(TaggedPassword),
    Bare(String),
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaggedPassword {
    #[serde(alias = "plaintext")]
    Plain(String),
    Argon2(String),
}

impl fmt::Debug for RawPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RawPassword(***)")
    }
}

impl Drop for RawPassword {
    fn drop(&mut self) {
        match self {
            RawPassword::Bare(s)
            | RawPassword::Tagged(TaggedPassword::Plain(s))
            | RawPassword::Tagged(TaggedPassword::Argon2(s)) => s.zeroize(),
        }
    }
}
