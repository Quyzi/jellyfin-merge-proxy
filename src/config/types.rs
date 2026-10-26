use serde::Deserialize;
use std::fmt;
use std::path::PathBuf;
use zeroize::Zeroize;

use crate::error::ConfigError;

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

pub use crate::password::{HashedPassword, PasswordCache};

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum JournalMode {
    #[default]
    Wal,
    Delete,
    Truncate,
    Persist,
    Memory,
    Off,
}

impl JournalMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Wal => "wal",
            Self::Delete => "delete",
            Self::Truncate => "truncate",
            Self::Persist => "persist",
            Self::Memory => "memory",
            Self::Off => "off",
        }
    }
}

impl std::fmt::Display for JournalMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Synchronous {
    Off,
    #[default]
    Normal,
    Full,
    Extra,
}

impl Synchronous {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Normal => "normal",
            Self::Full => "full",
            Self::Extra => "extra",
        }
    }

    pub fn as_pragma_value(&self) -> &'static str {
        match self {
            Self::Off => "0",
            Self::Normal => "1",
            Self::Full => "2",
            Self::Extra => "3",
        }
    }
}

impl std::fmt::Display for Synchronous {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

fn default_busy_timeout_ms() -> u64 {
    5000
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub journal_mode: JournalMode,
    #[serde(default)]
    pub synchronous: Synchronous,
    #[serde(default = "default_busy_timeout_ms")]
    pub busy_timeout_ms: u64,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: None,
            journal_mode: JournalMode::default(),
            synchronous: Synchronous::default(),
            busy_timeout_ms: default_busy_timeout_ms(),
        }
    }
}
