//! Copies of one series (and its seasons) that live on several servers.
//!
//! When dedupe folds a series found on two servers into one entry, the copy
//! that is shown keeps its own proxy id, and the other copies are recorded
//! here as its *siblings*. Season and episode lists for any member are then
//! fanned out to every sibling and merged (seasons by number, episodes by
//! season + episode number), so a show split across servers browses as one.
//!
//! Groups are persisted to `<data_dir>/siblings.tsv` (one `id\tid` pair per
//! line) so that ids held by clients keep their merged view across restarts.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, RwLock};

use crate::store::{SiblingStore, StoreError};
use crate::telemetry::names;
use crate::types::ProxyItemId;
use metrics::counter;

#[derive(Default)]
pub struct SiblingTable {
    groups: RwLock<HashMap<ProxyItemId, Arc<Vec<ProxyItemId>>>>,
    /// Ids already looked up on the other servers (not persisted).
    checked: Mutex<HashSet<ProxyItemId>>,
    store: Option<Arc<dyn SiblingStore>>,
}

impl SiblingTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load the links saved in `store` and save new links to it.
    pub fn with_store(mut self, store: Arc<dyn SiblingStore>) -> Result<Self, StoreError> {
        let links = store.load_links()?;
        for (a, b) in &links {
            self.union(&[a.clone(), b.clone()]);
        }
        tracing::info!("loaded {} sibling links from the state store", links.len());
        counter!(names::PERSIST_OPS, "store" => "siblings", "op" => "load", "outcome" => "success")
            .increment(1);
        self.store = Some(store);
        Ok(self)
    }

    /// Merge the groups of `ids` into one. Returns true if anything changed.
    fn union(&self, ids: &[ProxyItemId]) -> bool {
        let mut g = self.groups.write().unwrap();
        let mut all: BTreeSet<ProxyItemId> = BTreeSet::new();
        for id in ids {
            match g.get(id) {
                Some(members) => all.extend(members.iter().cloned()),
                None => {
                    all.insert(id.clone());
                }
            }
        }
        if all.len() < 2 {
            return false;
        }
        let unchanged = ids
            .iter()
            .all(|id| g.get(id).is_some_and(|m| m.len() == all.len()));
        if unchanged {
            return false;
        }
        let members = Arc::new(all.into_iter().collect::<Vec<_>>());
        for m in members.iter() {
            g.insert(m.clone(), members.clone());
        }
        true
    }

    /// Record that `ids` are copies of the same thing on different servers.
    pub fn link(&self, ids: &[ProxyItemId]) {
        if !self.union(ids) {
            return;
        }
        if let Some(store) = &self.store {
            let links: Vec<_> = ids[1..]
                .iter()
                .map(|other| (ids[0].clone(), other.clone()))
                .collect();
            let outcome = match store.append_links(&links) {
                Ok(()) => "success",
                Err(e) => {
                    tracing::warn!("saving sibling links failed: {e}");
                    "error"
                }
            };
            counter!(names::PERSIST_OPS, "store" => "siblings", "op" => "save", "outcome" => outcome)
                .increment(1);
        }
    }

    /// Every member of `id`'s group, including `id`; None if it has no siblings.
    pub fn group(&self, id: &ProxyItemId) -> Option<Arc<Vec<ProxyItemId>>> {
        self.groups.read().unwrap().get(id).cloned()
    }

    /// Returns true the first time it is called for `id`.
    pub fn first_check(&self, id: &ProxyItemId) -> bool {
        self.checked.lock().unwrap().insert(id.clone())
    }

    pub fn len(&self) -> usize {
        self.groups.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> ProxyItemId {
        ProxyItemId::parse(&format!("{n:032x}")).unwrap()
    }

    #[test]
    fn union_is_transitive_and_persisted() {
        let store = Arc::new(crate::store::MemoryStore::new());
        let t = SiblingTable::new().with_store(store.clone()).unwrap();
        assert!(t.group(&id(1)).is_none());
        t.link(&[id(1), id(2)]);
        t.link(&[id(3), id(2)]);
        t.link(&[id(1), id(3)]);
        let g = t.group(&id(3)).unwrap();
        assert_eq!(g.as_slice(), &[id(1), id(2), id(3)]);
        assert!(Arc::ptr_eq(&g, &t.group(&id(1)).unwrap()));
        drop(t);
        use crate::store::SiblingStore;
        assert_eq!(
            store.load_links().unwrap().len(),
            2,
            "no-op links are not written"
        );
        let t = SiblingTable::new().with_store(store).unwrap();
        assert_eq!(t.group(&id(2)).unwrap().len(), 3);
        assert!(t.first_check(&id(9)));
        assert!(!t.first_check(&id(9)));
    }

    #[test]
    fn links_round_trip_through_a_store() {
        use crate::store::{MemoryStore, SiblingStore};
        let store = Arc::new(MemoryStore::new());
        let t = SiblingTable::new().with_store(store.clone()).unwrap();
        t.link(&[id(1), id(2), id(3)]);
        t.link(&[id(2), id(1)]);
        assert_eq!(
            store.load_links().unwrap(),
            vec![(id(1), id(2)), (id(1), id(3))],
            "no-op links are not saved"
        );
        let t = SiblingTable::new().with_store(store).unwrap();
        assert_eq!(t.group(&id(3)).unwrap().len(), 3);
    }
}
