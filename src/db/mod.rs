//! SQLite persistence layer for ids, sessions, siblings, and catalog cache.
//!
//! Replaces the legacy .tsv and .json files with a single jmp.db database.
//! Supports configurable journal_mode and synchronous settings for compatibility
//! with different filesystem types (including network/FUSE mounts).

use crate::config::{DatabaseConfig, JournalMode, Synchronous};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

mod schema;
pub use schema::{init_db, migrate};

/// Thread-safe database connection wrapper.
#[derive(Clone)]
pub struct Database {
    inner: Arc<Mutex<Connection>>,
    config: DatabaseConfig,
}

impl Database {
    /// Open or create a database with the given configuration.
    pub fn open(data_dir: &Path, config: DatabaseConfig) -> Result<Self> {
        let path = config
            .path
            .clone()
            .unwrap_or_else(|| data_dir.join("jmp.db"));

        info!("Opening database at {}", path.display());

        let conn = Connection::open(&path)
            .with_context(|| format!("Failed to open database at {}", path.display()))?;

        // Apply configuration PRAGMAs
        Self::apply_pragmas(&conn, &config)?;

        // Check if the actual journal mode matches the requested mode
        Self::verify_journal_mode(&conn, &config)?;

        // Initialize schema and run migrations
        init_db(&conn)?;

        let db = Self {
            inner: Arc::new(Mutex::new(conn)),
            config,
        };

        // Export metrics
        metrics::gauge!(
            "jmp_db_journal_mode",
            "mode" => db.config.journal_mode.as_str()
        )
        .set(1);

        Ok(db)
    }

    fn apply_pragmas(conn: &Connection, config: &DatabaseConfig) -> Result<()> {
        // Set journal mode
        conn.pragma_update(None, "journal_mode", config.journal_mode.as_str())
            .context("Failed to set journal_mode")?;

        // Set synchronous mode
        conn.pragma_update(None, "synchronous", config.synchronous.as_pragma_value())
            .context("Failed to set synchronous")?;

        // Set busy timeout
        conn.pragma_update(None, "busy_timeout", config.busy_timeout_ms)
            .context("Failed to set busy_timeout")?;

        info!(
            "Applied SQLite settings: journal_mode={}, synchronous={}, busy_timeout={}ms",
            config.journal_mode, config.synchronous, config.busy_timeout_ms
        );

        Ok(())
    }

    fn verify_journal_mode(conn: &Connection, config: &DatabaseConfig) -> Result<()> {
        let actual: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .context("Failed to read back journal_mode")?;

        let expected = config.journal_mode.as_str();
        if actual.to_lowercase() != expected {
            warn!(
                "SQLite journal_mode mismatch: requested '{}', got '{}'. \
                 This can happen on network filesystems (e.g. Unraid user shares) \
                 that don't support WAL mode. Consider setting journal_mode='delete' \
                 in [database] config section.",
                expected, actual
            );
        }

        Ok(())
    }

    /// Execute a closure with a locked connection.
    pub fn with_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let conn = self.inner.lock().unwrap();
        f(&*conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn database_opens_with_defaults() {
        let dir = tempdir().unwrap();
        let config = DatabaseConfig::default();
        let db = Database::open(dir.path(), config).unwrap();

        db.with_conn(|conn| {
            let mode: String = conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
            assert!(!mode.is_empty());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn journal_mode_setting_applied() {
        let dir = tempdir().unwrap();
        let mut config = DatabaseConfig::default();
        config.journal_mode = JournalMode::Delete;

        let db = Database::open(dir.path(), config).unwrap();

        db.with_conn(|conn| {
            let mode: String = conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
            assert_eq!(mode.to_lowercase(), "delete");
            Ok(())
        })
        .unwrap();
    }
}
