//! Versioned schema migrations, embedded in the binary.
//!
//! The applied version is SQLite's `PRAGMA user_version`. Each migration
//! runs in its own transaction together with the version bump, so a crash
//! leaves the database at the previous version, never half-migrated. A
//! database written by a newer binary (higher version) is refused.

use super::StoreError;
use rusqlite::Connection;

pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

/// Every migration, in order. Append only; never edit a released one.
pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "init",
    sql: include_str!("migrations/0001_init.sql"),
}];

/// Schema version this binary writes.
pub fn latest() -> u32 {
    MIGRATIONS.last().map(|m| m.version).unwrap_or(0)
}

pub fn current_version(conn: &Connection) -> Result<u32, StoreError> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// Bring the schema up to [`latest`]. Returns the resulting version.
pub fn migrate(conn: &mut Connection) -> Result<u32, StoreError> {
    let found = current_version(conn)?;
    if found > latest() {
        return Err(StoreError::SchemaTooNew {
            found,
            supported: latest(),
        });
    }
    for m in MIGRATIONS.iter().filter(|m| m.version > found) {
        let tx = conn.transaction()?;
        tx.execute_batch(m.sql)?;
        tx.pragma_update(None, "user_version", m.version)?;
        tx.commit()?;
        tracing::info!(
            version = m.version,
            name = m.name,
            "applied database migration"
        );
    }
    current_version(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tables(conn: &Connection) -> Vec<String> {
        let mut st = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn versions_are_contiguous_from_one() {
        for (i, m) in MIGRATIONS.iter().enumerate() {
            assert_eq!(m.version as usize, i + 1, "{}", m.name);
        }
    }

    #[test]
    fn fresh_database_is_migrated_once() {
        let mut conn = Connection::open_in_memory().unwrap();
        assert_eq!(current_version(&conn).unwrap(), 0);
        assert_eq!(migrate(&mut conn).unwrap(), latest());
        assert_eq!(tables(&conn), ["ids", "sessions", "sibling_links"]);
        // Running again is a no-op (the CREATE TABLEs would fail otherwise).
        assert_eq!(migrate(&mut conn).unwrap(), latest());
    }

    #[test]
    fn newer_schema_is_refused() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", latest() + 1)
            .unwrap();
        let e = migrate(&mut conn).unwrap_err();
        assert!(matches!(e, StoreError::SchemaTooNew { .. }), "{e}");
        assert!(tables(&conn).is_empty());
    }

    #[test]
    fn failed_migration_rolls_back() {
        let mut conn = Connection::open_in_memory().unwrap();
        // A table in the way makes migration 1 fail half way.
        conn.execute_batch("CREATE TABLE sessions (x)").unwrap();
        assert!(migrate(&mut conn).is_err());
        assert_eq!(current_version(&conn).unwrap(), 0);
        assert_eq!(tables(&conn), ["sessions"]);
    }
}
