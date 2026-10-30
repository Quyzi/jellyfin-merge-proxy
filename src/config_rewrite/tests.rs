use super::*;
use crate::config::PasswordCache;
use crate::password::HashedPassword;
use crate::secrets::{EncryptionKey, XChaCha20Cipher};
use std::path::PathBuf;
use tempfile::tempdir;

#[test]
fn rewrite_preserves_formatting() {
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("config.toml");
    let toml = r#"# My config
[[user]]
name = "alice"
password = "secret123"  # her password

[[server]]
name = "home"
server = "http://localhost:8096"
user = "admin"
password = "admin123"  # admin pass
api_key = "abc123"  # API key
"#;
    fs::write(&cfg_path, toml).unwrap();

    let cache = PasswordCache::default();
    let key = EncryptionKey::generate();
    let cipher = XChaCha20Cipher::new(key);

    assert!(rewrite_secrets(&cfg_path, &cache, &cipher).unwrap());

    let rewritten = fs::read_to_string(&cfg_path).unwrap();
    println!("Rewritten:\n{}", rewritten);
    assert!(rewritten.contains("# My config"));
    // Inline comments should be preserved on rewritten values
    assert!(rewritten.contains("# her password"));
    assert!(rewritten.contains("# admin pass"));
    assert!(rewritten.contains("# API key"));
    assert!(rewritten.contains("$argon2id$"));
    assert!(rewritten.contains("enc:v1:"));
    assert!(!rewritten.contains("secret123"));
    assert!(!rewritten.contains("admin123"));
    assert!(!rewritten.contains("abc123"));
}

#[test]
fn rewrite_is_idempotent() {
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("config.toml");
    let toml = r#"
[[user]]
name = "bob"
password = "pw"

[[server]]
name = "srv"
server = "http://localhost:8096"
user = "u"
password = "p"
"#;
    fs::write(&cfg_path, toml).unwrap();

    let cache = PasswordCache::default();
    let key = EncryptionKey::generate();
    let cipher = XChaCha20Cipher::new(key.clone());

    assert!(rewrite_secrets(&cfg_path, &cache, &cipher).unwrap());
    let first = fs::read_to_string(&cfg_path).unwrap();

    // Second rewrite should do nothing
    assert!(!rewrite_secrets(&cfg_path, &cache, &cipher).unwrap());
    let second = fs::read_to_string(&cfg_path).unwrap();
    assert_eq!(first, second);
}

#[test]
fn rewrite_already_hashed_password_untouched() {
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("config.toml");
    let hash = HashedPassword::hash("secret").unwrap();
    let toml = format!(
        r#"
[[user]]
name = "carol"
password = "{}"
"#,
        hash.phc()
    );
    fs::write(&cfg_path, &toml).unwrap();

    let cache = PasswordCache::default();
    let key = EncryptionKey::generate();
    let cipher = XChaCha20Cipher::new(key);

    assert!(!rewrite_secrets(&cfg_path, &cache, &cipher).unwrap());
    let unchanged = fs::read_to_string(&cfg_path).unwrap();
    assert_eq!(unchanged.trim(), toml.trim());
}

#[test]
fn encrypted_secret_decrypts_correctly() {
    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("config.toml");
    let toml = r#"
[[server]]
name = "test"
server = "http://localhost:8096"
user = "admin"
password = "mypassword"
api_key = "myapikey"
"#;
    fs::write(&cfg_path, toml).unwrap();

    let cache = PasswordCache::default();
    let key = EncryptionKey::generate();
    let cipher = XChaCha20Cipher::new(key.clone());

    assert!(rewrite_secrets(&cfg_path, &cache, &cipher).unwrap());

    let rewritten = fs::read_to_string(&cfg_path).unwrap();
    let doc: DocumentMut = rewritten.parse().unwrap();
    let servers = doc.get("server").unwrap().as_array_of_tables().unwrap();
    let srv = servers.iter().next().unwrap();

    let encrypted_password = srv.get("password").unwrap().as_str().unwrap();
    let encrypted_api_key = srv.get("api_key").unwrap().as_str().unwrap();

    assert_eq!(cipher.decrypt(encrypted_password).unwrap(), "mypassword");
    assert_eq!(cipher.decrypt(encrypted_api_key).unwrap(), "myapikey");
}

#[test]
#[cfg(unix)]
fn fallback_to_inplace_on_readonly_parent() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    let cfg_path = dir.path().join("config.toml");
    let toml = r#"
[[user]]
name = "test"
password = "plain"
"#;
    fs::write(&cfg_path, toml).unwrap();

    // Make the directory read-only so we can't create temp files
    let mut perms = dir.path().metadata().unwrap().permissions();
    perms.set_mode(0o555);
    fs::set_permissions(dir.path(), perms.clone()).unwrap();

    let cache = PasswordCache::default();
    let key = EncryptionKey::generate();
    let cipher = XChaCha20Cipher::new(key);

    // Should fall back to in-place write and succeed
    let result = rewrite_secrets(&cfg_path, &cache, &cipher);

    // Restore permissions for cleanup
    perms.set_mode(0o755);
    fs::set_permissions(dir.path(), perms).unwrap();

    assert!(result.is_ok());
    assert!(result.unwrap());

    let rewritten = fs::read_to_string(&cfg_path).unwrap();
    assert!(rewritten.contains("$argon2id$"));
}

#[test]
fn should_fallback_on_permission_errors() {
    use std::io;

    // EXDEV (cross-device link)
    let err = ConfigError::Io {
        source: io::Error::from_raw_os_error(libc::EXDEV),
        path: PathBuf::from("/tmp/test"),
    };
    assert!(should_fallback_to_inplace(&err));

    // EACCES (permission denied)
    let err = ConfigError::Io {
        source: io::Error::from_raw_os_error(libc::EACCES),
        path: PathBuf::from("/tmp/test"),
    };
    assert!(should_fallback_to_inplace(&err));

    // EPERM (operation not permitted)
    let err = ConfigError::Io {
        source: io::Error::from_raw_os_error(libc::EPERM),
        path: PathBuf::from("/tmp/test"),
    };
    assert!(should_fallback_to_inplace(&err));

    // EROFS (read-only filesystem)
    let err = ConfigError::Io {
        source: io::Error::from_raw_os_error(libc::EROFS),
        path: PathBuf::from("/tmp/test"),
    };
    assert!(should_fallback_to_inplace(&err));

    // EBUSY (device or resource busy)
    let err = ConfigError::Io {
        source: io::Error::from_raw_os_error(libc::EBUSY),
        path: PathBuf::from("/tmp/test"),
    };
    assert!(should_fallback_to_inplace(&err));

    // Other errors should not trigger fallback
    let err = ConfigError::Io {
        source: io::Error::from_raw_os_error(libc::ENOENT),
        path: PathBuf::from("/tmp/test"),
    };
    assert!(!should_fallback_to_inplace(&err));
}
