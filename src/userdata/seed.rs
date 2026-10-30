//! One-time import of upstream watch state per (proxy user, server).
//!
//! When `[userdata] seed_from_upstream` is on, the first time a proxy user
//! and a server meet (after the index has crawled that server, so ids map
//! to canonical ids), the state the upstream account holds (played, play
//! counts, resume points, favourites, likes) is imported for that user,
//! never overwriting state the proxy already has. The pair is recorded so
//! it is imported only once.

use super::rules::from_upstream;
use super::ItemState;
use crate::backend::MediaBackend;
use crate::store::Store;
use crate::telemetry::names;
use crate::types::ProxyItemId;
use crate::Shared;
use metrics::counter;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// Upstream filters whose items carry state worth importing.
const FILTERS: &[&str] = &["IsPlayed", "IsResumable", "IsFavorite", "Likes", "Dislikes"];

pub struct Seeder {
    store: Arc<dyn Store>,
    done: Mutex<HashSet<(String, String)>>,
}

/// Merge two states of copies of one item.
fn merge(a: &mut ItemState, b: ItemState) {
    a.played |= b.played;
    a.favorite |= b.favorite;
    a.play_count = a.play_count.max(b.play_count);
    if b.last_played > a.last_played {
        a.last_played = b.last_played;
        if b.position_ticks > 0 {
            a.position_ticks = b.position_ticks;
        }
    } else if a.position_ticks == 0 {
        a.position_ticks = b.position_ticks;
    }
    a.likes = a.likes.or(b.likes);
    a.rating = a.rating.or(b.rating);
}

impl Seeder {
    pub fn new(store: Arc<dyn Store>) -> Self {
        let done = store
            .load_seeded()
            .map(|v| v.into_iter().collect())
            .unwrap_or_else(|e| {
                tracing::warn!("loading user data seed marks failed: {e}");
                HashSet::new()
            });
        Seeder {
            store,
            done: Mutex::new(done),
        }
    }

    fn is_done(&self, user: &str, server: &str) -> bool {
        self.done
            .lock()
            .unwrap()
            .contains(&(user.to_string(), server.to_string()))
    }

    /// Seed every (user, live and indexed server) pair not seeded yet.
    pub async fn pass(&self, st: &Shared) {
        let cfg = st.cfg();
        if !cfg.userdata.enabled || !cfg.userdata.seed_from_upstream {
            return;
        }
        let set = st.backends();
        // Wait for the index so ids map to their canonical ids.
        if cfg.index.enabled && st.index.snapshot(&cfg, &set[..]).is_none() {
            return;
        }
        let users = st.authenticator.ids();
        for up in set.iter().filter(|b| !b.is_down()) {
            let pending: Vec<&String> = users
                .iter()
                .filter(|u| !self.is_done(u, up.name()))
                .collect();
            if pending.is_empty() {
                continue;
            }
            let rows = match fetch(st, up.as_ref()).await {
                Ok(r) => r,
                Err(e) => {
                    counter!(names::USERDATA_SEEDS, "outcome" => "error").increment(1);
                    tracing::warn!(upstream = %up.name(), "importing user data failed: {e}");
                    continue;
                }
            };
            for u in pending {
                let n = st.userdata.table().seed(u, rows.clone());
                if let Err(e) = self.store.mark_seeded(u, up.name()) {
                    tracing::warn!("saving user data seed mark failed: {e}");
                }
                self.done
                    .lock()
                    .unwrap()
                    .insert((u.clone(), up.name().to_string()));
                counter!(names::USERDATA_SEEDS, "outcome" => "ok").increment(1);
                tracing::info!(upstream = %up.name(), items = n, "imported upstream user data");
            }
        }
    }
}

/// Every item with state on `up` (as the account the proxy uses), keyed by
/// canonical proxy id.
async fn fetch(
    st: &Shared,
    up: &dyn MediaBackend,
) -> anyhow::Result<Vec<(ProxyItemId, ItemState)>> {
    up.ensure_login().await?;
    let uc = up.session().await;
    let uid = uc.user_id.clone().unwrap_or_default();
    let page = st.cfg().index.page_size;
    let mut out: HashMap<ProxyItemId, ItemState> = HashMap::new();
    for f in FILTERS {
        let mut start = 0usize;
        loop {
            let q = format!(
                "Users/{uid}/Items?Recursive=true&Filters={f}&EnableUserData=true\
                 &EnableImages=false&StartIndex={start}&Limit={page}"
            );
            let (status, v) = up.get_json(&q).await?;
            if !status.is_success() {
                anyhow::bail!("HTTP {status}");
            }
            let items = v.get("Items").and_then(Value::as_array).cloned();
            let items = items.unwrap_or_default();
            for it in &items {
                let (Some(id), Some(ud)) =
                    (it.get("Id").and_then(Value::as_str), it.get("UserData"))
                else {
                    continue;
                };
                let pid = st.ids.to_proxy(up.id(), id);
                let Some(pid) = ProxyItemId::parse(&pid) else {
                    continue;
                };
                let mut s = from_upstream(ud);
                if it.get("IsFolder").and_then(Value::as_bool) == Some(true) {
                    // Folder played state is derived from its episodes.
                    s.played = false;
                    s.play_count = 0;
                    s.position_ticks = 0;
                }
                let key = st.userdata.key(&pid);
                match out.get_mut(&key) {
                    Some(a) => merge(a, s),
                    None => {
                        out.insert(key, s);
                    }
                }
            }
            start += items.len();
            let total = v
                .get("TotalRecordCount")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            if items.is_empty() || start >= total {
                break;
            }
        }
    }
    Ok(out.into_iter().collect())
}
