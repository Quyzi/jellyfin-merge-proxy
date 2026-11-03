//! Each proxy user's [`RatingPolicy`], resolved from config and the index.

use super::{allows_item, RatingPolicy};
use crate::config::{Config, RatingLimit};
use crate::index::service::IndexService;
use crate::index::snapshot::Snapshot;
use crate::telemetry::names;
use crate::types::ProxyItemId;
use arc_swap::ArcSwap;
use metrics::counter;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq)]
struct UserLimit {
    limit: RatingLimit,
    block_unrated: bool,
}

fn limits_of(cfg: &Config) -> HashMap<String, UserLimit> {
    cfg.users
        .iter()
        .filter_map(|u| {
            let limit = u.max_parental_rating.clone()?;
            Some((
                crate::users::user_id_for(&u.name),
                UserLimit {
                    limit,
                    block_unrated: u.block_unrated,
                },
            ))
        })
        .collect()
}

pub struct ParentalService {
    limits: ArcSwap<HashMap<String, UserLimit>>,
    index: Arc<IndexService>,
    /// Finds the server of an id the index does not hold.
    ids: Arc<dyn crate::ids::IdMapper>,
    warned: Mutex<HashSet<String>>,
}

impl ParentalService {
    pub fn new(cfg: &Config, index: Arc<IndexService>, ids: Arc<dyn crate::ids::IdMapper>) -> Self {
        ParentalService {
            limits: ArcSwap::from_pointee(limits_of(cfg)),
            index,
            ids,
            warned: Mutex::new(HashSet::new()),
        }
    }

    /// Whether `user` has a limit at all.
    pub fn restricted(&self, user: &str) -> bool {
        self.limits.load().contains_key(user)
    }

    /// `user`'s policy (None = unrestricted). A rating name the servers'
    /// tables do not know (or no index yet) fails closed to score 0.
    pub fn policy(&self, user: &str, snap: Option<&Snapshot>) -> Option<RatingPolicy> {
        let limits = self.limits.load();
        let l = limits.get(user)?;
        let max = match &l.limit {
            RatingLimit::Score(s) => *s,
            RatingLimit::Name(n) => {
                let found = snap.and_then(|s| s.ratings.get(&n.to_lowercase()).copied());
                found.unwrap_or_else(|| {
                    if snap.is_some() && self.warned.lock().unwrap().insert(n.clone()) {
                        tracing::warn!(rating = %n, "max_parental_rating not in any server's rating table; allowing only score 0");
                    }
                    0
                })
            }
        };
        Some(RatingPolicy {
            max,
            block_unrated: l.block_unrated,
        })
    }

    /// `policy` against the latest index snapshot.
    pub fn current(&self, user: &str) -> Option<(RatingPolicy, Option<Arc<Snapshot>>)> {
        if !self.restricted(user) {
            return None;
        }
        let snap = self.index.latest();
        let p = self.policy(user, snap.as_deref())?;
        Some((p, snap))
    }

    /// Whether `user` may not see item `id`. Indexed items are judged by
    /// rating. For a restricted user this fails closed: before the first
    /// index exists everything is blocked, and an id the index does not
    /// hold is blocked when its server is not in the snapshot (added,
    /// down at start, or still being crawled for the first time). Other
    /// unknown ids (kinds the index does not keep, items newer than the last
    /// crawl) pass; fan-out lists are filtered by
    /// [`super::filter::ParentalFilter`].
    pub fn blocks(&self, user: &str, id: &ProxyItemId) -> bool {
        let Some((p, snap)) = self.current(user) else {
            return false;
        };
        let Some(snap) = snap else {
            counter!(names::PARENTAL_HIDDEN, "source" => "item").increment(1);
            return true;
        };
        let blocked = match snap.item_index(id) {
            Some(i) => !allows_item(&p, &snap, i),
            None => matches!(
                self.ids.decode(id),
                Some(crate::ids::Target::Item { upstream, .. }) if !snap.covers(upstream)
            ),
        };
        if blocked {
            counter!(names::PARENTAL_HIDDEN, "source" => "item").increment(1);
        }
        blocked
    }

    /// Drop DTOs `user` may not see from a fan-out page (items are rewritten
    /// one by one there, so the rewriter never sees the list); returns how
    /// many were dropped.
    pub fn retain_dtos(&self, user: &str, items: &mut Vec<serde_json::Value>) -> usize {
        let Some((p, snap)) = self.current(user) else {
            return 0;
        };
        let hidden = super::filter::retain_page(&p, snap.as_deref(), items);
        if hidden > 0 {
            counter!(names::PARENTAL_HIDDEN, "source" => "list").increment(hidden as u64);
        }
        hidden
    }

    /// Drop items `user` may not see from index results (order kept).
    pub fn retain(&self, user: &str, snap: &Snapshot, hits: &mut Vec<usize>) {
        let Some(p) = self.policy(user, Some(snap)) else {
            return;
        };
        let before = hits.len();
        hits.retain(|i| allows_item(&p, snap, *i));
        let hidden = before - hits.len();
        if hidden > 0 {
            counter!(names::PARENTAL_HIDDEN, "source" => "index").increment(hidden as u64);
        }
    }
}

#[async_trait::async_trait]
impl crate::reload::Reloadable for ParentalService {
    fn reload_name(&self) -> String {
        "parental".into()
    }
    async fn apply(
        &self,
        cx: &crate::reload::ReloadContext<'_>,
    ) -> Result<bool, crate::error::ReloadError> {
        let new = limits_of(cx.new);
        if **self.limits.load() == new {
            return Ok(false);
        }
        self.limits.store(Arc::new(new));
        self.warned.lock().unwrap().clear();
        Ok(true)
    }
}
