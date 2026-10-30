//! `[database]` section: where and how the SQLite state database is opened.

use super::*;

/// SQLite `journal_mode`. `wal` (the default) lets readers and the writer
/// run concurrently; the others exist for filesystems where WAL's shared
/// memory file does not work (some network mounts).
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
    pub const ALL: [JournalMode; 6] = [
        JournalMode::Wal,
        JournalMode::Delete,
        JournalMode::Truncate,
        JournalMode::Persist,
        JournalMode::Memory,
        JournalMode::Off,
    ];

    /// The pragma value, which is also how SQLite reports the mode back.
    pub fn as_str(self) -> &'static str {
        match self {
            JournalMode::Wal => "wal",
            JournalMode::Delete => "delete",
            JournalMode::Truncate => "truncate",
            JournalMode::Persist => "persist",
            JournalMode::Memory => "memory",
            JournalMode::Off => "off",
        }
    }

    /// Parse a mode as written in the config or returned by
    /// `PRAGMA journal_mode` (case-insensitive).
    pub fn parse(s: &str) -> Option<JournalMode> {
        Self::ALL
            .into_iter()
            .find(|m| m.as_str().eq_ignore_ascii_case(s.trim()))
    }
}

impl fmt::Display for JournalMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// SQLite `synchronous` setting. `normal` (the default) is safe with WAL:
/// a power loss can drop the last commits but never corrupts the database.
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
    pub fn as_str(self) -> &'static str {
        match self {
            Synchronous::Off => "off",
            Synchronous::Normal => "normal",
            Synchronous::Full => "full",
            Synchronous::Extra => "extra",
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    /// Database file. Default: `<data_dir>/jmp.db`.
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub journal_mode: JournalMode,
    #[serde(default)]
    pub synchronous: Synchronous,
    /// How long a statement waits for a lock held by another connection.
    #[serde(default = "default_busy_timeout_ms")]
    pub busy_timeout_ms: u64,
}

fn default_busy_timeout_ms() -> u64 {
    5000
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        DatabaseConfig {
            path: None,
            journal_mode: JournalMode::default(),
            synchronous: Synchronous::default(),
            busy_timeout_ms: default_busy_timeout_ms(),
        }
    }
}

impl DatabaseConfig {
    /// The database file for a given `data_dir`.
    pub fn resolved_path(&self, data_dir: &Path) -> PathBuf {
        self.path.clone().unwrap_or_else(|| data_dir.join("jmp.db"))
    }
}
