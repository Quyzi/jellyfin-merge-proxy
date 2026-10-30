//! [`MemoryStore`]: a [`Store`] that keeps everything in process memory.

use super::*;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

/// Non-durable [`Store`]: state lives as long as the value.
#[derive(Default)]
pub struct MemoryStore {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    ids: Vec<IdRow>,
    id_keys: HashSet<ProxyItemId>,
    links: Vec<(ProxyItemId, ProxyItemId)>,
    link_keys: HashSet<(ProxyItemId, ProxyItemId)>,
    /// Sessions in insertion order (by token).
    sessions: Vec<String>,
    session_map: HashMap<String, Session>,
    index: super::index::MemIndex,
    userdata: super::userdata::MemUserData,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl IdStore for MemoryStore {
    fn load_ids(&self) -> Result<Vec<IdRow>, StoreError> {
        Ok(self.inner.lock().unwrap().ids.clone())
    }

    fn append_ids(&self, rows: &[IdRow]) -> Result<(), StoreError> {
        let mut g = self.inner.lock().unwrap();
        for r in rows {
            if g.id_keys.insert(r.proxy_id.clone()) {
                g.ids.push(r.clone());
            }
        }
        Ok(())
    }
}

impl SiblingStore for MemoryStore {
    fn load_links(&self) -> Result<Vec<(ProxyItemId, ProxyItemId)>, StoreError> {
        Ok(self.inner.lock().unwrap().links.clone())
    }

    fn append_links(&self, links: &[(ProxyItemId, ProxyItemId)]) -> Result<(), StoreError> {
        let mut g = self.inner.lock().unwrap();
        for l in links {
            if g.link_keys.insert(l.clone()) {
                g.links.push(l.clone());
            }
        }
        Ok(())
    }
}

impl SessionStore for MemoryStore {
    fn load_sessions(&self) -> Result<Vec<Session>, StoreError> {
        let g = self.inner.lock().unwrap();
        Ok(g.sessions
            .iter()
            .filter_map(|t| g.session_map.get(t).cloned())
            .collect())
    }

    fn put_session(&self, s: &Session) -> Result<(), StoreError> {
        let mut g = self.inner.lock().unwrap();
        if g.session_map.insert(s.token.clone(), s.clone()).is_none() {
            g.sessions.push(s.token.clone());
        }
        Ok(())
    }

    fn remove_sessions(&self, tokens: &[String]) -> Result<(), StoreError> {
        let mut g = self.inner.lock().unwrap();
        for t in tokens {
            g.session_map.remove(t);
        }
        let Inner {
            sessions,
            session_map,
            ..
        } = &mut *g;
        sessions.retain(|t| session_map.contains_key(t));
        Ok(())
    }
}

impl IndexStore for MemoryStore {
    fn load_index(&self) -> Result<Vec<StoredServerIndex>, StoreError> {
        Ok(self.inner.lock().unwrap().index.load_index())
    }
    fn save_server_index(&self, idx: &StoredServerIndex) -> Result<(), StoreError> {
        self.inner.lock().unwrap().index.save_server_index(idx);
        Ok(())
    }
    fn load_canonical(&self) -> Result<Vec<(ProxyItemId, ProxyItemId)>, StoreError> {
        Ok(self.inner.lock().unwrap().index.load_canonical())
    }
    fn save_canonical(&self, rows: &[(ProxyItemId, ProxyItemId)]) -> Result<(), StoreError> {
        self.inner.lock().unwrap().index.save_canonical(rows);
        Ok(())
    }
    fn load_views(&self) -> Result<Vec<StoredView>, StoreError> {
        Ok(self.inner.lock().unwrap().index.load_views())
    }
    fn save_view(&self, view: &StoredView) -> Result<(), StoreError> {
        self.inner.lock().unwrap().index.save_view(view);
        Ok(())
    }
}

impl UserDataStore for MemoryStore {
    fn load_user_data(&self) -> Result<Vec<UserDataRow>, StoreError> {
        Ok(self.inner.lock().unwrap().userdata.load())
    }
    fn put_user_data(&self, rows: &[UserDataRow]) -> Result<(), StoreError> {
        self.inner.lock().unwrap().userdata.put(rows);
        Ok(())
    }
    fn load_seeded(&self) -> Result<Vec<(String, String)>, StoreError> {
        Ok(self.inner.lock().unwrap().userdata.seeded())
    }
    fn mark_seeded(&self, user: &str, server: &str) -> Result<(), StoreError> {
        self.inner.lock().unwrap().userdata.mark(user, server);
        Ok(())
    }
}

impl Store for MemoryStore {
    fn kind(&self) -> &'static str {
        "memory"
    }
}
