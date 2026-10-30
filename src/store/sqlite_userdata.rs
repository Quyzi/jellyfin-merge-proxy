//! [`UserDataStore`] for [`SqliteStore`].

use super::userdata::{UserDataRow, UserDataStore};
use super::{SqliteStore, StoreError};
use crate::types::ProxyItemId;
use crate::userdata::ItemState;
use rusqlite::params;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl UserDataStore for SqliteStore {
    fn load_user_data(&self) -> Result<Vec<UserDataRow>, StoreError> {
        self.read(|c| {
            let mut st = c.prepare("SELECT user_id, item_id, state FROM user_data")?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            let mut out = vec![];
            for row in rows {
                let (user, item, state) = row?;
                let Some(item) = ProxyItemId::parse(&item) else {
                    continue;
                };
                match serde_json::from_str::<ItemState>(&state) {
                    Ok(state) => out.push(UserDataRow { user, item, state }),
                    Err(e) => tracing::warn!("skipping user data row: {e}"),
                }
            }
            Ok(out)
        })
    }

    fn put_user_data(&self, rows: &[UserDataRow]) -> Result<(), StoreError> {
        let ts = now();
        self.write(|c| {
            let mut st = c.prepare_cached(
                "INSERT OR REPLACE INTO user_data (user_id, item_id, state, updated_at) \
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for r in rows {
                let state = serde_json::to_string(&r.state)
                    .map_err(|e| StoreError::Invalid(format!("user data json: {e}")))?;
                st.execute(params![r.user, r.item.as_str(), state, ts])?;
            }
            Ok(())
        })
    }

    fn load_seeded(&self) -> Result<Vec<(String, String)>, StoreError> {
        self.read(|c| {
            let mut st = c.prepare("SELECT user_id, server FROM user_data_seeded")?;
            let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })
    }

    fn mark_seeded(&self, user: &str, server: &str) -> Result<(), StoreError> {
        self.write(|c| {
            c.execute(
                "INSERT OR REPLACE INTO user_data_seeded (user_id, server, seeded_at) \
                 VALUES (?1, ?2, ?3)",
                params![user, server, now()],
            )?;
            Ok(())
        })
    }
}
