//! [`SqliteStore`]: the durable [`Store`], one SQLite file
//! (`<data_dir>/jmp.db` by default).

use super::*;
use crate::config::{DatabaseConfig, JournalMode};
use crate::telemetry::names;
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

/// [`Store`] backed by SQLite. One connection behind a mutex: writes are
/// small and infrequent (new ids are batched by the id table's writer
/// thread), and reads only happen at startup.
pub struct SqliteStore {
    conn: Mutex<Connection>,
    path: PathBuf,
    journal_mode: JournalMode,
    schema_version: u32,
}

impl SqliteStore {
    /// Open (creating if needed) and migrate the database at `path`.
    pub fn open(path: &Path, cfg: &DatabaseConfig) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        restrict_permissions(path)?;
        Self::init(conn, path.to_path_buf(), cfg)
    }

    /// A private in-memory database (tests).
    pub fn open_in_memory(cfg: &DatabaseConfig) -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?, ":memory:".into(), cfg)
    }

    fn init(mut conn: Connection, path: PathBuf, cfg: &DatabaseConfig) -> Result<Self, StoreError> {
        conn.busy_timeout(Duration::from_millis(cfg.busy_timeout_ms))?;
        let journal_mode = apply_journal_mode(&conn, cfg.journal_mode)?;
        conn.pragma_update(None, "synchronous", cfg.synchronous.as_str())?;
        let schema_version = migrations::migrate(&mut conn)?;
        publish_journal_mode(journal_mode);
        metrics::gauge!(names::DB_SCHEMA_VERSION).set(schema_version as f64);
        tracing::info!(
            path = %path.display(),
            %journal_mode,
            synchronous = cfg.synchronous.as_str(),
            schema_version,
            "opened state database"
        );
        Ok(SqliteStore {
            conn: Mutex::new(conn),
            path,
            journal_mode,
            schema_version,
        })
    }

    /// The journal mode SQLite actually uses (see [`apply_journal_mode`]).
    pub fn journal_mode(&self) -> JournalMode {
        self.journal_mode
    }

    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Request `wanted` and return the mode SQLite reports back. SQLite keeps
/// a different mode without an error when the requested one is not
/// possible (WAL on an in-memory database, or on a filesystem without
/// shared-memory support), so a mismatch is logged as a warning.
pub fn apply_journal_mode(
    conn: &Connection,
    wanted: JournalMode,
) -> Result<JournalMode, StoreError> {
    let got: String =
        conn.pragma_update_and_check(None, "journal_mode", wanted.as_str(), |r| r.get(0))?;
    let effective = JournalMode::parse(&got).ok_or_else(|| {
        StoreError::Invalid(format!("SQLite reported unknown journal_mode {got:?}"))
    })?;
    if effective != wanted {
        tracing::warn!(
            requested = %wanted,
            %effective,
            "SQLite did not accept [database] journal_mode; using {effective}"
        );
    }
    Ok(effective)
}

/// `jmp_db_journal_mode{mode}`: 1 for the effective mode, 0 for the others.
pub fn publish_journal_mode(effective: JournalMode) {
    for m in JournalMode::ALL {
        let v = if m == effective { 1.0 } else { 0.0 };
        metrics::gauge!(names::DB_JOURNAL_MODE, "mode" => m.as_str()).set(v);
    }
}

/// The database holds session tokens: owner read/write only.
fn restrict_permissions(path: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
            |source| StoreError::Io {
                path: path.to_path_buf(),
                source,
            },
        )?;
    }
    let _ = path;
    Ok(())
}

fn insert_ids(c: &Connection, rows: &[IdRow]) -> Result<(), StoreError> {
    let mut st = c.prepare_cached(
        "INSERT OR IGNORE INTO ids (proxy_id, server, upstream_id) VALUES (?1, ?2, ?3)",
    )?;
    for r in rows {
        st.execute(params![
            r.proxy_id.as_str(),
            r.server,
            r.upstream_id.as_str()
        ])?;
    }
    Ok(())
}

fn insert_links(c: &Connection, links: &[(ProxyItemId, ProxyItemId)]) -> Result<(), StoreError> {
    let mut st = c.prepare_cached("INSERT OR IGNORE INTO sibling_links (a, b) VALUES (?1, ?2)")?;
    for (a, b) in links {
        st.execute(params![a.as_str(), b.as_str()])?;
    }
    Ok(())
}

fn upsert_session(c: &Connection, s: &Session) -> Result<(), StoreError> {
    c.prepare_cached(
        "INSERT INTO sessions (token, session_id, user_name, user_id, client, device_name, \
         device_id, app_version, created) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
         ON CONFLICT (token) DO UPDATE SET session_id = excluded.session_id, \
         user_name = excluded.user_name, user_id = excluded.user_id, client = excluded.client, \
         device_name = excluded.device_name, device_id = excluded.device_id, \
         app_version = excluded.app_version, created = excluded.created",
    )?
    .execute(params![
        s.token,
        s.session_id,
        s.user_name,
        s.user_id,
        s.client,
        s.device_name,
        s.device_id,
        s.app_version,
        s.created
    ])?;
    Ok(())
}

impl SqliteStore {
    /// Run `f` in one transaction.
    fn write<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut c = self.conn.lock().unwrap();
        let tx = c.transaction()?;
        let r = f(&tx)?;
        tx.commit()?;
        Ok(r)
    }
}

impl IdStore for SqliteStore {
    fn load_ids(&self) -> Result<Vec<IdRow>, StoreError> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare("SELECT proxy_id, server, upstream_id FROM ids ORDER BY seq")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (p, server, u) = row?;
            match (ProxyItemId::parse(&p), UpstreamItemId::parse(&u)) {
                (Some(proxy_id), Some(upstream_id)) => out.push(IdRow {
                    proxy_id,
                    server,
                    upstream_id,
                }),
                _ => tracing::warn!(proxy_id = %p, upstream_id = %u, "skipping malformed id row"),
            }
        }
        Ok(out)
    }

    fn append_ids(&self, rows: &[IdRow]) -> Result<(), StoreError> {
        self.write(|c| insert_ids(c, rows))
    }
}

impl SiblingStore for SqliteStore {
    fn load_links(&self) -> Result<Vec<(ProxyItemId, ProxyItemId)>, StoreError> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare("SELECT a, b FROM sibling_links ORDER BY seq")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (a, b) = row?;
            if let (Some(a), Some(b)) = (ProxyItemId::parse(&a), ProxyItemId::parse(&b)) {
                out.push((a, b));
            }
        }
        Ok(out)
    }

    fn append_links(&self, links: &[(ProxyItemId, ProxyItemId)]) -> Result<(), StoreError> {
        self.write(|c| insert_links(c, links))
    }
}

impl SessionStore for SqliteStore {
    fn load_sessions(&self) -> Result<Vec<Session>, StoreError> {
        let c = self.conn.lock().unwrap();
        let mut st = c.prepare(
            "SELECT token, session_id, user_name, user_id, client, device_name, device_id, \
             app_version, created FROM sessions ORDER BY rowid",
        )?;
        let rows = st.query_map([], |r| {
            Ok(Session {
                token: r.get(0)?,
                session_id: r.get(1)?,
                user_name: r.get(2)?,
                user_id: r.get(3)?,
                client: r.get(4)?,
                device_name: r.get(5)?,
                device_id: r.get(6)?,
                app_version: r.get(7)?,
                created: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn put_session(&self, s: &Session) -> Result<(), StoreError> {
        self.write(|c| upsert_session(c, s))
    }

    fn remove_sessions(&self, tokens: &[String]) -> Result<(), StoreError> {
        self.write(|c| {
            let mut st = c.prepare_cached("DELETE FROM sessions WHERE token = ?1")?;
            for t in tokens {
                st.execute([t])?;
            }
            Ok(())
        })
    }
}

impl Store for SqliteStore {
    fn kind(&self) -> &'static str {
        "sqlite"
    }

    fn import(&self, batch: &ImportBatch) -> Result<(), StoreError> {
        self.write(|c| {
            insert_ids(c, &batch.ids)?;
            insert_links(c, &batch.links)?;
            batch.sessions.iter().try_for_each(|s| upsert_session(c, s))
        })
    }
}
