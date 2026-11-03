//! [`LocalStore`] for [`SqliteStore`].

use super::local::LocalStore;
use super::{SqliteStore, StoreError};
use rusqlite::params;
use serde_json::Value;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn parse(s: &str) -> Option<Value> {
    match serde_json::from_str(s) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!("skipping local row: {e}");
            None
        }
    }
}

fn json(v: &Value) -> String {
    v.to_string()
}

impl LocalStore for SqliteStore {
    fn load_settings(&self) -> Result<Vec<(String, Value)>, StoreError> {
        self.read(|c| {
            let mut st = c.prepare("SELECT user_id, config FROM user_settings")?;
            let rows =
                st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut out = vec![];
            for row in rows {
                let (user, config) = row?;
                if let Some(v) = parse(&config) {
                    out.push((user, v));
                }
            }
            Ok(out)
        })
    }

    fn put_settings(&self, user: &str, v: &Value) -> Result<(), StoreError> {
        self.write(|c| {
            c.execute(
                "INSERT OR REPLACE INTO user_settings (user_id, config, updated_at) \
                 VALUES (?1, ?2, ?3)",
                params![user, json(v), now()],
            )?;
            Ok(())
        })
    }

    fn load_display_prefs(&self) -> Result<Vec<(String, String, String, Value)>, StoreError> {
        self.read(|c| {
            let mut st = c.prepare("SELECT user_id, prefs_id, client, data FROM display_prefs")?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?;
            let mut out = vec![];
            for row in rows {
                let (user, prefs, client, data) = row?;
                if let Some(v) = parse(&data) {
                    out.push((user, prefs, client, v));
                }
            }
            Ok(out)
        })
    }

    fn put_display_prefs(
        &self,
        user: &str,
        prefs: &str,
        client: &str,
        v: &Value,
    ) -> Result<(), StoreError> {
        self.write(|c| {
            c.execute(
                "INSERT OR REPLACE INTO display_prefs (user_id, prefs_id, client, data, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![user, prefs, client, json(v), now()],
            )?;
            Ok(())
        })
    }

    fn load_playlists(&self) -> Result<Vec<Value>, StoreError> {
        self.read(|c| {
            let mut st = c.prepare("SELECT data FROM playlists ORDER BY id")?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            let mut out = vec![];
            for row in rows {
                if let Some(v) = parse(&row?) {
                    out.push(v);
                }
            }
            Ok(out)
        })
    }

    fn put_playlist(&self, id: &str, owner: &str, v: &Value) -> Result<(), StoreError> {
        self.write(|c| {
            c.execute(
                "INSERT OR REPLACE INTO playlists (id, owner_user_id, data, updated_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, owner, json(v), now()],
            )?;
            Ok(())
        })
    }

    fn delete_playlist(&self, id: &str) -> Result<(), StoreError> {
        self.write(|c| {
            c.execute("DELETE FROM playlists WHERE id = ?1", params![id])?;
            Ok(())
        })
    }
}
