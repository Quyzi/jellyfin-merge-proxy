//! Which copy of a grouped item is the best one to show and play.
//!
//! [`CopyRanker`] is the seam (`[index] prefer`): [`PriorityRanker`]
//! (`"priority"`, the default: lowest `[[server]] priority`, then config
//! order) and [`ResolutionRanker`] (`"resolution"`: most pixels first,
//! then priority). At request time the best copy on a *live* server is
//! used, so a server that is down never hides an item that exists elsewhere.

use super::snapshot::CopyRec;
use crate::types::ServerId;
use std::cmp::Ordering;

/// What a ranker knows about a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ServerRank {
    pub priority: i64,
    /// Position in the config (ties on priority).
    pub order: usize,
}

pub trait CopyRanker: Send + Sync {
    fn name(&self) -> &'static str;
    /// `Less` = `a` is the better copy.
    fn compare(&self, a: &CopyRec, b: &CopyRec, rank: &dyn Fn(ServerId) -> ServerRank) -> Ordering;
}

fn by_server(a: &CopyRec, b: &CopyRec, rank: &dyn Fn(ServerId) -> ServerRank) -> Ordering {
    let (ra, rb) = (rank(a.server), rank(b.server));
    (ra.priority, ra.order, a.server).cmp(&(rb.priority, rb.order, b.server))
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PriorityRanker;

impl CopyRanker for PriorityRanker {
    fn name(&self) -> &'static str {
        "priority"
    }
    fn compare(&self, a: &CopyRec, b: &CopyRec, rank: &dyn Fn(ServerId) -> ServerRank) -> Ordering {
        by_server(a, b, rank)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ResolutionRanker;

impl CopyRanker for ResolutionRanker {
    fn name(&self) -> &'static str {
        "resolution"
    }
    fn compare(&self, a: &CopyRec, b: &CopyRec, rank: &dyn Fn(ServerId) -> ServerRank) -> Ordering {
        b.entry
            .pixels()
            .cmp(&a.entry.pixels())
            .then_with(|| by_server(a, b, rank))
    }
}

/// The ranker named by `[index] prefer`.
pub fn ranker(name: &str) -> Box<dyn CopyRanker> {
    match name {
        "resolution" => Box::new(ResolutionRanker),
        _ => Box::new(PriorityRanker),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::entry::Entry;
    use crate::types::ProxyItemId;

    fn copy(server: usize, w: i64) -> CopyRec {
        CopyRec {
            server: ServerId(server),
            pid: ProxyItemId::parse(&"a".repeat(32)).unwrap(),
            entry: Entry {
                width: Some(w),
                height: Some(w / 2),
                ..Default::default()
            },
        }
    }

    #[test]
    fn rankers_order_copies() {
        let rank = |s: ServerId| ServerRank {
            priority: if s.0 == 1 { 0 } else { 10 },
            order: s.0,
        };
        let (a, b) = (copy(0, 1920), copy(1, 640));
        assert_eq!(PriorityRanker.compare(&a, &b, &rank), Ordering::Greater);
        assert_eq!(ResolutionRanker.compare(&a, &b, &rank), Ordering::Less);
        assert_eq!(ranker("resolution").name(), "resolution");
        assert_eq!(ranker("anything").name(), "priority");
    }
}
