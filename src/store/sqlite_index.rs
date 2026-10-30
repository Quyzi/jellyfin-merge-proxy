//! [`IndexStore`] for [`SqliteStore`].

use super::index::{IndexStore, StoredServerIndex, StoredView};
use super::{SqliteStore, StoreError};
use crate::index::entry::Entry;
use crate::types::ProxyItemId;
use rusqlite::params;

fn json_err(e: serde_json::Error) -> StoreError {
    StoreError::Invalid(format!("index json: {e}"))
}

impl IndexStore for SqliteStore {
    fn load_index(&self) -> Result<Vec<StoredServerIndex>, StoreError> {
        self.read(|c| {
            let mut out: Vec<StoredServerIndex> = vec![];
            let mut st =
                c.prepare("SELECT server, synced_at, ratings FROM index_servers ORDER BY server")?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (server, synced_at, ratings) = row?;
                out.push(StoredServerIndex {
                    server,
                    synced_at,
                    ratings: serde_json::from_str(&ratings).unwrap_or_default(),
                    entries: vec![],
                });
            }
            let mut st = c.prepare("SELECT data FROM index_entries WHERE server = ?1")?;
            for s in out.iter_mut() {
                let rows = st.query_map([&s.server], |r| r.get::<_, String>(0))?;
                for data in rows {
                    match serde_json::from_str::<Entry>(&data?) {
                        Ok(e) => s.entries.push(e),
                        Err(e) => tracing::warn!(server = %s.server, "skipping index row: {e}"),
                    }
                }
            }
            Ok(out)
        })
    }

    fn save_server_index(&self, idx: &StoredServerIndex) -> Result<(), StoreError> {
        let ratings = serde_json::to_string(&idx.ratings).map_err(json_err)?;
        self.write(|c| {
            c.execute("DELETE FROM index_entries WHERE server = ?1", [&idx.server])?;
            c.execute(
                "INSERT INTO index_servers (server, synced_at, ratings) VALUES (?1, ?2, ?3) \
                 ON CONFLICT (server) DO UPDATE SET synced_at = excluded.synced_at, \
                 ratings = excluded.ratings",
                params![idx.server, idx.synced_at, ratings],
            )?;
            let mut st = c.prepare_cached(
                "INSERT OR REPLACE INTO index_entries (server, upstream_id, data) \
                 VALUES (?1, ?2, ?3)",
            )?;
            for e in &idx.entries {
                let data = serde_json::to_string(e).map_err(json_err)?;
                st.execute(params![idx.server, e.id, data])?;
            }
            Ok(())
        })
    }

    fn load_canonical(&self) -> Result<Vec<(ProxyItemId, ProxyItemId)>, StoreError> {
        self.read(|c| {
            let mut st = c.prepare("SELECT member, canonical FROM canonical_ids")?;
            let rows =
                st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut out = vec![];
            for row in rows {
                let (m, k) = row?;
                if let (Some(m), Some(k)) = (ProxyItemId::parse(&m), ProxyItemId::parse(&k)) {
                    out.push((m, k));
                }
            }
            Ok(out)
        })
    }

    fn save_canonical(&self, rows: &[(ProxyItemId, ProxyItemId)]) -> Result<(), StoreError> {
        self.write(|c| {
            let mut st = c.prepare_cached(
                "INSERT INTO canonical_ids (member, canonical) VALUES (?1, ?2) \
                 ON CONFLICT (member) DO UPDATE SET canonical = excluded.canonical",
            )?;
            for (m, k) in rows {
                st.execute(params![m.as_str(), k.as_str()])?;
            }
            Ok(())
        })
    }

    fn load_views(&self) -> Result<Vec<StoredView>, StoreError> {
        self.read(|c| {
            let mut st = c.prepare("SELECT data FROM merged_views ORDER BY proxy_id")?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            let mut out = vec![];
            for data in rows {
                match serde_json::from_str::<StoredView>(&data?) {
                    Ok(v) => out.push(v),
                    Err(e) => tracing::warn!("skipping merged view row: {e}"),
                }
            }
            Ok(out)
        })
    }

    fn save_view(&self, view: &StoredView) -> Result<(), StoreError> {
        let data = serde_json::to_string(view).map_err(json_err)?;
        self.write(|c| {
            c.execute(
                "INSERT OR REPLACE INTO merged_views (proxy_id, data) VALUES (?1, ?2)",
                params![view.id.as_str(), data],
            )?;
            Ok(())
        })
    }
}
