//! [`UserDataTable`] and [`MeteredWatchState`].

use super::{ItemState, WatchState};
use crate::store::{Store, UserDataRow};
use crate::telemetry::names;
use crate::types::ProxyItemId;
use metrics::{counter, gauge};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

type UserMap = HashMap<ProxyItemId, ItemState>;

/// In-memory state of every user, loaded from and written through to a
/// store.
pub struct UserDataTable {
    users: RwLock<HashMap<String, UserMap>>,
    store: Arc<dyn Store>,
}

impl UserDataTable {
    pub fn load(store: Arc<dyn Store>) -> Self {
        let mut users: HashMap<String, UserMap> = HashMap::new();
        match store.load_user_data() {
            Ok(rows) => {
                for r in rows {
                    users.entry(r.user).or_default().insert(r.item, r.state);
                }
            }
            Err(e) => tracing::warn!("loading user data failed: {e}"),
        }
        UserDataTable {
            users: RwLock::new(users),
            store,
        }
    }

    fn persist(&self, user: &str, rows: Vec<(ProxyItemId, ItemState)>) {
        let rows: Vec<UserDataRow> = rows
            .into_iter()
            .map(|(item, state)| UserDataRow {
                user: user.to_string(),
                item,
                state,
            })
            .collect();
        if let Err(e) = self.store.put_user_data(&rows) {
            counter!(names::USERDATA_STORE_ERRORS).increment(1);
            tracing::warn!("saving user data failed: {e}");
        }
    }
}

impl WatchState for UserDataTable {
    fn get(&self, user: &str, item: &ProxyItemId) -> Option<ItemState> {
        self.users.read().unwrap().get(user)?.get(item).cloned()
    }

    fn update(
        &self,
        user: &str,
        items: &[ProxyItemId],
        _op: &'static str,
        f: &dyn Fn(&mut ItemState),
    ) -> Vec<ItemState> {
        let mut changed = vec![];
        let out = {
            let mut g = self.users.write().unwrap();
            let m = g.entry(user.to_string()).or_default();
            items
                .iter()
                .map(|id| {
                    let s = m.entry(id.clone()).or_default();
                    f(s);
                    changed.push((id.clone(), s.clone()));
                    s.clone()
                })
                .collect()
        };
        self.persist(user, changed);
        out
    }

    fn seed(&self, user: &str, rows: Vec<(ProxyItemId, ItemState)>) -> usize {
        let mut fresh = vec![];
        {
            let mut g = self.users.write().unwrap();
            let m = g.entry(user.to_string()).or_default();
            for (id, s) in rows {
                if !m.contains_key(&id) {
                    m.insert(id.clone(), s.clone());
                    fresh.push((id, s));
                }
            }
        }
        let n = fresh.len();
        if n > 0 {
            self.persist(user, fresh);
        }
        n
    }

    fn user_items(&self, user: &str) -> Vec<(ProxyItemId, ItemState)> {
        self.users
            .read()
            .unwrap()
            .get(user)
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default()
    }

    fn len(&self) -> usize {
        self.users.read().unwrap().values().map(|m| m.len()).sum()
    }
}

/// Counts changes (`jmp_userdata_changes_total{op}`) and keeps
/// `jmp_userdata_items` current.
pub struct MeteredWatchState<W>(pub W);

impl<W: WatchState> MeteredWatchState<W> {
    /// Wrap `inner`, publishing its current size.
    pub fn new(inner: W) -> Self {
        gauge!(names::USERDATA_ITEMS).set(inner.len() as f64);
        MeteredWatchState(inner)
    }
}

impl<W: WatchState> WatchState for MeteredWatchState<W> {
    fn get(&self, user: &str, item: &ProxyItemId) -> Option<ItemState> {
        self.0.get(user, item)
    }

    fn update(
        &self,
        user: &str,
        items: &[ProxyItemId],
        op: &'static str,
        f: &dyn Fn(&mut ItemState),
    ) -> Vec<ItemState> {
        let out = self.0.update(user, items, op, f);
        counter!(names::USERDATA_CHANGES, "op" => op).increment(items.len() as u64);
        gauge!(names::USERDATA_ITEMS).set(self.0.len() as f64);
        out
    }

    fn seed(&self, user: &str, rows: Vec<(ProxyItemId, ItemState)>) -> usize {
        let n = self.0.seed(user, rows);
        counter!(names::USERDATA_CHANGES, "op" => "seed").increment(n as u64);
        gauge!(names::USERDATA_ITEMS).set(self.0.len() as f64);
        n
    }

    fn user_items(&self, user: &str) -> Vec<(ProxyItemId, ItemState)> {
        self.0.user_items(user)
    }

    fn len(&self) -> usize {
        self.0.len()
    }
}
