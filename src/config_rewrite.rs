//! Config file rewriting: hash user passwords and encrypt upstream secrets,
//! preserving TOML formatting, comments, and order.
//!
//! Uses `toml_edit` to modify the file in place, then writes atomically via
//! a temp file + rename (or in-place for bind-mounted files where rename fails).

use crate::config::{PasswordCache, RawPassword, TaggedPassword};
use crate::error::ConfigError;
use crate::secrets::SecretCipher;
use crate::telemetry::names;
use metrics::counter;
use std::fs;
use std::io::Write;
use std::path::Path;
use toml_edit::{DocumentMut, Item, Value};

/// Rewrites a TOML config file, hashing user passwords and encrypting upstream
/// secrets. Returns true if the file was modified.
pub fn rewrite_secrets(
    path: &Path,
    cache: &PasswordCache,
    cipher: &dyn SecretCipher,
) -> Result<bool, ConfigError> {
    let text = fs::read_to_string(path).map_err(|e| ConfigError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| ConfigError::Invalid(format!("failed to parse TOML for rewriting: {e}")))?;

    let mut modified = false;

    // Hash [[user]] passwords
    if let Some(users) = doc.get_mut("user").and_then(|v| v.as_array_of_tables_mut()) {
        for user_table in users.iter_mut() {
            let name = match user_table.get("name").and_then(|v| v.as_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            if let Some(password_item) = user_table.get_mut("password") {
                if let Some(new_hash) = rewrite_user_password(password_item, &name, cache)? {
                    if let Some(val) = password_item.as_value_mut() {
                        let decor = val.decor().clone();
                        *val = Value::from(new_hash);
                        *val.decor_mut() = decor;
                        modified = true;
                        counter!(names::SECRETS_REWRITTEN, "kind" => "user_password").increment(1);
                    }
                }
            }
            modified |= rewrite_user_accounts(user_table, cipher)?;
        }
    }

    // Encrypt [[server]] passwords and api_keys
    if let Some(servers) = doc
        .get_mut("server")
        .and_then(|v| v.as_array_of_tables_mut())
    {
        for server_table in servers.iter_mut() {
            if let Some(password_item) = server_table.get_mut("password") {
                if let Some(encrypted) = rewrite_secret(password_item, cipher)? {
                    if let Some(val) = password_item.as_value_mut() {
                        let decor = val.decor().clone();
                        *val = Value::from(encrypted);
                        *val.decor_mut() = decor;
                        modified = true;
                        counter!(names::SECRETS_REWRITTEN, "kind" => "upstream_password")
                            .increment(1);
                    }
                }
            }
            if let Some(api_key_item) = server_table.get_mut("api_key") {
                if let Some(encrypted) = rewrite_secret(api_key_item, cipher)? {
                    if let Some(val) = api_key_item.as_value_mut() {
                        let decor = val.decor().clone();
                        *val = Value::from(encrypted);
                        *val.decor_mut() = decor;
                        modified = true;
                        counter!(names::SECRETS_REWRITTEN, "kind" => "upstream_api_key")
                            .increment(1);
                    }
                }
            }
        }
    }

    if !modified {
        return Ok(false);
    }

    // Write atomically
    write_atomic(path, doc.to_string().as_bytes())?;
    Ok(true)
}

/// Encrypt `password` / `api_key` in each `[[user.account]]` (array of
/// tables or an inline `account = [{ ... }]` array).
fn rewrite_user_accounts(
    user: &mut toml_edit::Table,
    cipher: &dyn SecretCipher,
) -> Result<bool, ConfigError> {
    let mut modified = false;
    let fields = [
        ("password", "user_account_password"),
        ("api_key", "user_account_api_key"),
    ];
    let Some(item) = user.get_mut("account") else {
        return Ok(false);
    };
    if let Some(tables) = item.as_array_of_tables_mut() {
        for t in tables.iter_mut() {
            for (key, kind) in fields {
                if let Some(it) = t.get_mut(key) {
                    if let Some(enc) = rewrite_secret(it, cipher)? {
                        if let Some(val) = it.as_value_mut() {
                            replace_keep_decor(val, enc);
                            modified = true;
                            counter!(names::SECRETS_REWRITTEN, "kind" => kind).increment(1);
                        }
                    }
                }
            }
        }
    } else if let Some(arr) = item.as_array_mut() {
        for v in arr.iter_mut() {
            let Some(t) = v.as_inline_table_mut() else {
                continue;
            };
            for (key, kind) in fields {
                if let Some(val) = t.get_mut(key) {
                    let plain = match val.as_str() {
                        Some(s) if !cipher.is_encrypted(s) => s.to_string(),
                        _ => continue,
                    };
                    replace_keep_decor(val, cipher.encrypt(&plain)?);
                    modified = true;
                    counter!(names::SECRETS_REWRITTEN, "kind" => kind).increment(1);
                }
            }
        }
    }
    Ok(modified)
}

fn replace_keep_decor(val: &mut Value, new: String) {
    let decor = val.decor().clone();
    *val = Value::from(new);
    *val.decor_mut() = decor;
}

/// Replace one proxy user's `password` with `phc`, keeping formatting.
/// The user is matched case-insensitively on `name`/`username` in the
/// `user`/`users` array of tables. `Ok(false)` means no such user.
pub fn set_user_password(path: &Path, user: &str, phc: &str) -> Result<bool, ConfigError> {
    let text = fs::read_to_string(path).map_err(|e| ConfigError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| ConfigError::Invalid(format!("failed to parse TOML for rewriting: {e}")))?;
    let mut found = false;
    for key in ["user", "users"] {
        let Some(users) = doc.get_mut(key).and_then(|v| v.as_array_of_tables_mut()) else {
            continue;
        };
        for t in users.iter_mut() {
            let hit = ["name", "username"].iter().any(|k| {
                t.get(k)
                    .and_then(|v| v.as_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case(user))
            });
            if !hit {
                continue;
            }
            match t.get_mut("password").and_then(|i| i.as_value_mut()) {
                Some(v) => replace_keep_decor(v, phc.to_string()),
                None => {
                    t.insert("password", toml_edit::value(phc));
                }
            }
            found = true;
        }
    }
    if found {
        write_atomic(path, doc.to_string().as_bytes())?;
    }
    Ok(found)
}

/// Hash a user password if it's plaintext. Returns `Some(hash)` if modified.
fn rewrite_user_password(
    item: &Item,
    user: &str,
    cache: &PasswordCache,
) -> Result<Option<String>, ConfigError> {
    let raw = parse_raw_password(item)?;
    match &raw {
        RawPassword::Tagged(TaggedPassword::Argon2(_)) => Ok(None),
        RawPassword::Bare(s) if s.starts_with("$argon2") => Ok(None),
        _ => {
            let hashed = cache
                .normalize(user, &raw)
                .map_err(|e| ConfigError::Password {
                    user: user.to_string(),
                    source: e,
                })?;
            Ok(Some(hashed.phc().to_string()))
        }
    }
}

/// Encrypt a secret if it's plaintext. Returns `Some(encrypted)` if modified.
fn rewrite_secret(item: &Item, cipher: &dyn SecretCipher) -> Result<Option<String>, ConfigError> {
    let plaintext = match item.as_str() {
        Some(s) if !cipher.is_encrypted(s) => s,
        _ => return Ok(None),
    };
    Ok(Some(cipher.encrypt(plaintext)?))
}

fn parse_raw_password(item: &Item) -> Result<RawPassword, ConfigError> {
    match item {
        Item::Value(Value::String(s)) => {
            let v = s.value();
            if v.starts_with("$argon2") {
                Ok(RawPassword::Bare(v.to_string()))
            } else {
                Ok(RawPassword::Tagged(TaggedPassword::Plain(v.to_string())))
            }
        }
        Item::Value(Value::InlineTable(t)) => {
            if let Some(plain) = t.get("plain").or_else(|| t.get("plaintext")) {
                if let Some(s) = plain.as_str() {
                    return Ok(RawPassword::Tagged(TaggedPassword::Plain(s.to_string())));
                }
            }
            if let Some(argon2) = t.get("argon2") {
                if let Some(s) = argon2.as_str() {
                    return Ok(RawPassword::Tagged(TaggedPassword::Argon2(s.to_string())));
                }
            }
            Err(ConfigError::Invalid(
                "password inline table must have 'plain' or 'argon2' key".into(),
            ))
        }
        _ => Err(ConfigError::Invalid(
            "password must be a string or inline table".into(),
        )),
    }
}

/// Write `contents` to `path` atomically. Tries temp file + rename; falls back
/// to in-place write for bind-mounted files or read-only directories.
fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), ConfigError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));

    // Try atomic rename first
    match write_via_temp(path, dir, contents) {
        Ok(()) => Ok(()),
        Err(e) if should_fallback_to_inplace(&e) => {
            // Bind-mounted file or read-only directory: fall back to in-place write
            tracing::warn!(
                path = %path.display(),
                "temp file creation failed (bind-mount or read-only dir); writing in place"
            );
            write_in_place(path, contents)?;
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn write_via_temp(path: &Path, dir: &Path, contents: &[u8]) -> Result<(), ConfigError> {
    let temp_name = format!(
        ".{}.tmp.{}",
        path.file_name().unwrap().to_string_lossy(),
        std::process::id()
    );
    let temp_path = dir.join(&temp_name);

    // Write to temp file
    let mut temp_file = fs::File::create(&temp_path).map_err(|e| ConfigError::Io {
        path: temp_path.clone(),
        source: e,
    })?;

    temp_file.write_all(contents).map_err(|e| ConfigError::Io {
        path: temp_path.clone(),
        source: e,
    })?;

    temp_file.sync_all().map_err(|e| ConfigError::Io {
        path: temp_path.clone(),
        source: e,
    })?;

    drop(temp_file);

    // Preserve original permissions
    #[cfg(unix)]
    if let Ok(meta) = fs::metadata(path) {
        let perms = meta.permissions();
        let _ = fs::set_permissions(&temp_path, perms);
    }

    // Atomic rename
    fs::rename(&temp_path, path).map_err(|e| {
        let _ = fs::remove_file(&temp_path);
        ConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        }
    })?;

    Ok(())
}

fn write_in_place(path: &Path, contents: &[u8]) -> Result<(), ConfigError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

    file.write_all(contents).map_err(|e| ConfigError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    file.sync_all().map_err(|e| ConfigError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;

    Ok(())
}

fn should_fallback_to_inplace(e: &ConfigError) -> bool {
    if let ConfigError::Io { source, .. } = e {
        matches!(
            source.raw_os_error(),
            Some(libc::EXDEV)   // Cross-device link (bind mount)
            | Some(libc::EBUSY) // Device or resource busy
            | Some(libc::EACCES) // Permission denied (e.g., /etc not writable by user)
            | Some(libc::EPERM)  // Operation not permitted
            | Some(libc::EROFS) // Read-only file system (systemd ProtectSystem=strict)
        )
    } else {
        false
    }
}

#[cfg(test)]
mod tests;
