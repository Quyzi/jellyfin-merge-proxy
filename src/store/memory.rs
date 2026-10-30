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

impl Store for MemoryStore {
    fn kind(&self) -> &'static str {
        "memory"
    }
}
