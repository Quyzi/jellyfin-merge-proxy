//! Database schema definitions and migrations.

use anyhow::{Context, Result};
use rusqlite::Connection;
use tracing::info;

const CURRENT_VERSION: i32 = 1;

/// Initialize the database: create schema if missing, run migrations if needed.
pub fn init_db(conn: &Connection) -> Result<()> {
    let version = get_schema_version(conn)?;

    if version == 0 {
        create_schema_v1(conn)?;
        set_schema_version(conn, 1)?;
        info!("Created database schema v1");
    } else if version < CURRENT_VERSION {
        migrate(conn, version, CURRENT_VERSION)?;
    } else if version > CURRENT_VERSION {
        anyhow::bail!(
            "Database schema version {} is newer than supported version {}. \
             Upgrade jellyfin-merge-proxy.",
            version,
            CURRENT_VERSION
        );
    }

    Ok(())
}

/// Migrate database from `from_version` to `to_version`.
pub fn migrate(conn: &Connection, from: i32, to: i32) -> Result<()> {
    info!("Migrating database from v{} to v{}", from, to);

    for v in from..to {
        match v {
            // Future migrations will go here
            _ => {
                anyhow::bail!("Unknown migration path from v{} to v{}", from, to);
            }
        }
    }

    set_schema_version(conn, to)?;
    Ok(())
}

fn get_schema_version(conn: &Connection) -> Result<i32> {
    let version: i32 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .context("Failed to read user_version")?;
    Ok(version)
}

fn set_schema_version(conn: &Connection, version: i32) -> Result<()> {
    conn.pragma_update(None, "user_version", version)
        .context("Failed to set user_version")?;
    Ok(())
}

fn create_schema_v1(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        -- ID mappings (replaces ids.tsv)
        CREATE TABLE id_map (
            merged_id TEXT NOT NULL,
            server_id INTEGER NOT NULL,
            upstream_id TEXT NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
            PRIMARY KEY (merged_id, server_id)
        ) STRICT;

        CREATE INDEX idx_id_map_server_upstream ON id_map(server_id, upstream_id);
        CREATE INDEX idx_id_map_upstream ON id_map(upstream_id);

        -- Sessions (replaces sessions.json)
        CREATE TABLE sessions (
            token TEXT PRIMARY KEY NOT NULL,
            user_name TEXT NOT NULL,
            device_id TEXT NOT NULL,
            device_name TEXT NOT NULL,
            client TEXT NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
            last_seen INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
        ) STRICT;

        CREATE INDEX idx_sessions_user ON sessions(user_name);
        CREATE INDEX idx_sessions_device ON sessions(device_id);

        -- Siblings (replaces siblings.tsv)
        CREATE TABLE siblings (
            id TEXT PRIMARY KEY NOT NULL,
            server_id INTEGER NOT NULL,
            sibling_ids TEXT NOT NULL, -- JSON array of sibling IDs
            discovered_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
        ) STRICT;

        CREATE INDEX idx_siblings_server ON siblings(server_id);

        -- Catalog cache (replaces per-server JSON files)
        CREATE TABLE catalog_cache (
            server_id INTEGER NOT NULL,
            view_id TEXT NOT NULL,
            items BLOB NOT NULL, -- JSON blob of items
            cached_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
            PRIMARY KEY (server_id, view_id)
        ) STRICT;

        CREATE INDEX idx_catalog_cache_time ON catalog_cache(cached_at);
        "#,
    )
    .context("Failed to create schema v1")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn schema_v1_creates_all_tables() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        let version = get_schema_version(&conn).unwrap();
        assert_eq!(version, 1);

        // Verify tables exist
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(tables.contains(&"id_map".to_string()));
        assert!(tables.contains(&"sessions".to_string()));
        assert!(tables.contains(&"siblings".to_string()));
        assert!(tables.contains(&"catalog_cache".to_string()));
    }

    #[test]
    fn idempotent_init() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        // Second init should succeed without changes
        init_db(&conn).unwrap();
        assert_eq!(get_schema_version(&conn).unwrap(), 1);
    }
}
