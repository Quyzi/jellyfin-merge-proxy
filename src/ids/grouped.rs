//! [`GroupedIdMapper`]: canonical ids for items that exist on several
//! servers.
//!
//! The catalog index groups copies of one item and gives the group a
//! canonical id (one of its members' proxy ids). This decorator makes any
//! member id usable everywhere:
//!
//! * `decode` of a canonical id is the group's best live copy (members are
//!   ranked by `[index] prefer`), so priority changes move playback;
//! * `decode` of another member is that copy, or a live one if its server
//!   is down;
//! * `resolve_for(id, server)` finds the group's copy on `server`, so
//!   queries such as `ParentId`/`SeriesId`/`GenreIds` fan out correctly.

use super::*;
use crate::index::groups::GroupLookup;
use std::sync::OnceLock;

/// Whether a server can currently serve requests.
pub type Liveness = Arc<dyn Fn(ServerId) -> bool + Send + Sync>;

pub struct GroupedIdMapper<M> {
    inner: M,
    groups: Arc<dyn GroupLookup>,
    live: OnceLock<Liveness>,
}

impl<M: IdMapper> GroupedIdMapper<M> {
    pub fn new(inner: M, groups: Arc<dyn GroupLookup>) -> Self {
        GroupedIdMapper {
            inner,
            groups,
            live: OnceLock::new(),
        }
    }

    /// Install the liveness check (once; later calls are ignored).
    pub fn set_liveness(&self, f: Liveness) {
        let _ = self.live.set(f);
    }

    fn is_live(&self, s: ServerId) -> bool {
        self.live.get().is_none_or(|f| f(s))
    }

    /// Members of `id`'s group (decoded), best first.
    fn members(&self, id: &ProxyItemId) -> Vec<(ServerId, UpstreamItemId)> {
        let Some(g) = self.groups.group(id) else {
            return vec![];
        };
        g.members
            .iter()
            .filter_map(|m| match self.inner.decode(m)? {
                Target::Item { upstream, id } => Some((upstream, id)),
                Target::View(_) => None,
            })
            .collect()
    }
}

impl<M: IdMapper> IdMapper for GroupedIdMapper<M> {
    fn encode(&self, server: ServerId, id: &UpstreamItemId) -> Encoded {
        self.inner.encode(server, id)
    }

    fn decode(&self, id: &ProxyItemId) -> Option<Target> {
        // The canonical id (the one clients see) is the best live copy.
        if let Some(g) = self.groups.group(id).filter(|g| g.canonical == *id) {
            let best = g.members.iter().find_map(|m| match self.inner.decode(m)? {
                Target::Item { upstream, id } if self.is_live(upstream) => {
                    Some(Target::Item { upstream, id })
                }
                _ => None,
            });
            if best.is_some() {
                return best;
            }
        }
        let t = self.inner.decode(id);
        match &t {
            Some(Target::Item { upstream, .. }) if !self.is_live(*upstream) => self
                .members(id)
                .into_iter()
                .find(|(s, _)| self.is_live(*s))
                .map(|(upstream, id)| Target::Item { upstream, id })
                .or(t),
            None => self
                .members(id)
                .into_iter()
                .next()
                .map(|(upstream, id)| Target::Item { upstream, id }),
            _ => t,
        }
    }

    fn resolve_for(&self, raw: &str, server: ServerId) -> Option<UpstreamItemId> {
        let pid = ProxyItemId::parse(raw)?;
        match self.inner.decode(&pid) {
            Some(Target::Item { upstream, id }) if upstream == server => return Some(id),
            Some(Target::View(v)) => {
                return v
                    .members
                    .iter()
                    .find(|(u, _)| *u == server)
                    .map(|(_, id)| id.clone())
            }
            _ => {}
        }
        self.members(&pid)
            .into_iter()
            .find(|(s, _)| *s == server)
            .map(|(_, id)| id)
    }

    fn register_view(&self, view: MergedView) -> Arc<MergedView> {
        self.inner.register_view(view)
    }
    fn size_of(&self, server: ServerId) -> u64 {
        self.inner.size_of(server)
    }
    fn sizes(&self) -> Vec<(String, u64)> {
        self.inner.sizes()
    }
    fn len(&self) -> usize {
        self.inner.len()
    }
    fn publish_gauges(&self) {
        self.inner.publish_gauges()
    }
}
