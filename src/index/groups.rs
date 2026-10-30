//! Canonical groups: proxy ids that stand for the same thing on several
//! servers (copies of a movie/series/season/episode, or a genre, studio or
//! person with the same name).
//!
//! Every group has one *canonical* id, the id clients see. It is chosen
//! once and persisted ([`crate::store::IndexStore`]), so it stays stable
//! when copies come and go: it is the id of one of the copies, and keeps
//! resolving (to the best live copy) even after that copy disappears.
//!
//! [`GroupLookup`] is the read seam used on the request path (by
//! [`crate::ids::GroupedIdMapper`] and the canonical id rewriter);
//! [`CanonicalGroups`] is the lock-free implementation the index publishes
//! to after each rebuild.

use crate::types::ProxyItemId;
use arc_swap::ArcSwap;
use std::collections::HashMap;
use std::sync::Arc;

/// One group: its canonical id and members, best copy first.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub canonical: ProxyItemId,
    pub members: Arc<[ProxyItemId]>,
}

/// Immutable group table. Only groups that change an id are stored: more
/// than one member, or a canonical id that is not the member's own.
#[derive(Debug, Default, Clone)]
pub struct GroupMap {
    /// By canonical id and by every member id.
    by_id: HashMap<ProxyItemId, Arc<Group>>,
    groups: usize,
}

impl GroupMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a group (ignored if it would not change any id).
    pub fn insert(&mut self, canonical: ProxyItemId, members: Vec<ProxyItemId>) {
        if members.is_empty() || (members.len() == 1 && members[0] == canonical) {
            return;
        }
        let g = Arc::new(Group {
            canonical: canonical.clone(),
            members: members.into(),
        });
        for m in g.members.iter() {
            self.by_id.insert(m.clone(), g.clone());
        }
        self.by_id.insert(canonical, g);
        self.groups += 1;
    }

    pub fn get(&self, id: &ProxyItemId) -> Option<&Arc<Group>> {
        self.by_id.get(id)
    }

    /// Number of stored groups.
    pub fn len(&self) -> usize {
        self.groups
    }

    pub fn is_empty(&self) -> bool {
        self.groups == 0
    }
}

/// Read access to the current groups.
pub trait GroupLookup: Send + Sync {
    /// The group `id` belongs to (as canonical or member), if any.
    fn group(&self, id: &ProxyItemId) -> Option<Arc<Group>>;

    /// The id clients should see for `id` (itself when ungrouped).
    fn canonical(&self, id: &ProxyItemId) -> ProxyItemId {
        self.group(id)
            .map(|g| g.canonical.clone())
            .unwrap_or_else(|| id.clone())
    }
}

/// [`GroupLookup`] over an atomically swapped [`GroupMap`].
#[derive(Default)]
pub struct CanonicalGroups {
    map: ArcSwap<GroupMap>,
}

impl CanonicalGroups {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn publish(&self, map: GroupMap) {
        self.map.store(Arc::new(map));
    }

    pub fn snapshot(&self) -> Arc<GroupMap> {
        self.map.load_full()
    }
}

impl GroupLookup for CanonicalGroups {
    fn group(&self, id: &ProxyItemId) -> Option<Arc<Group>> {
        self.map.load().get(id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(c: char) -> ProxyItemId {
        ProxyItemId::parse(&c.to_string().repeat(32)).unwrap()
    }

    #[test]
    fn lookup_by_member_and_canonical() {
        let mut m = GroupMap::new();
        m.insert(p('a'), vec![p('b'), p('a')]);
        m.insert(p('c'), vec![p('c')]); // no-op
        m.insert(p('d'), vec![p('e')]); // canonical of a vanished copy
        let g = CanonicalGroups::new();
        g.publish(m);
        assert_eq!(g.canonical(&p('b')), p('a'));
        assert_eq!(g.group(&p('a')).unwrap().members[0], p('b'));
        assert_eq!(g.canonical(&p('c')), p('c'));
        assert_eq!(g.canonical(&p('e')), p('d'));
        assert_eq!(g.group(&p('d')).unwrap().members.len(), 1);
        assert_eq!(g.snapshot().len(), 2);
    }
}
