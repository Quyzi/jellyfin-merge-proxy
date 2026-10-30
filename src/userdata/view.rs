//! Per-user `UserData` DTOs and the [`UserDataOverlay`] response rewriter.

use super::service::{FolderCounts, UserDataService};
use super::ItemState;
use crate::rewrite::{OutCtx, Payload, PayloadBody, ResponseRewriter};
use crate::types::ProxyItemId;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// Jellyfin's `UserItemDataDto` for `state`. `key` keeps the upstream's
/// `Key` when known.
pub fn dto(
    state: &ItemState,
    item_id: &str,
    key: Option<&str>,
    runtime: Option<i64>,
    counts: Option<FolderCounts>,
) -> Value {
    let mut m = Map::new();
    if let Some(r) = state.rating {
        m.insert("Rating".into(), json!(r));
    }
    match counts {
        Some(c) => {
            if c.total > 0 {
                let pct = (c.total - c.unplayed) as f64 * 100.0 / c.total as f64;
                m.insert("PlayedPercentage".into(), json!(pct));
            }
            m.insert("UnplayedItemCount".into(), json!(c.unplayed));
        }
        None => {
            if let Some(rt) = runtime.filter(|r| *r > 0 && state.position_ticks > 0) {
                let pct = state.position_ticks as f64 * 100.0 / rt as f64;
                m.insert("PlayedPercentage".into(), json!(pct));
            }
        }
    }
    m.insert("PlaybackPositionTicks".into(), json!(state.position_ticks));
    m.insert("PlayCount".into(), json!(state.play_count));
    m.insert("IsFavorite".into(), json!(state.favorite));
    if let Some(l) = state.likes {
        m.insert("Likes".into(), json!(l));
    }
    if let Some(d) = &state.last_played {
        m.insert("LastPlayedDate".into(), json!(d));
    }
    m.insert("Played".into(), json!(state.played));
    m.insert("Key".into(), json!(key.unwrap_or(item_id)));
    m.insert("ItemId".into(), json!(item_id));
    Value::Object(m)
}

/// The user's DTO for an item id (folder counts and runtime from the
/// index; `runtime` overrides when the caller has the item's DTO).
pub fn user_dto(
    svc: &UserDataService,
    user: &str,
    id: &ProxyItemId,
    key: Option<&str>,
    runtime: Option<i64>,
) -> Value {
    let (state, counts) = svc.effective(user, id);
    let rt = runtime.or_else(|| svc.runtime(id));
    dto(&state, id.as_str(), key, rt, counts)
}

/// Replaces every item's `UserData` in a response with the requesting
/// proxy user's state. Runs after the id rewriters (ids are proxy ids).
pub struct UserDataOverlay {
    svc: Arc<UserDataService>,
}

impl UserDataOverlay {
    pub fn new(svc: Arc<UserDataService>) -> Self {
        UserDataOverlay { svc }
    }

    pub fn apply(&self, v: &mut Value, user: &str) {
        match v {
            Value::Object(map) => {
                let id = map
                    .get("Id")
                    .and_then(Value::as_str)
                    .and_then(ProxyItemId::parse);
                if let (Some(id), Some(Value::Object(ud))) = (id, map.get("UserData")) {
                    let key = ud.get("Key").and_then(Value::as_str).map(String::from);
                    let rt = map.get("RunTimeTicks").and_then(Value::as_i64);
                    let new = user_dto(&self.svc, user, &id, key.as_deref(), rt);
                    map.insert("UserData".into(), new);
                }
                for (k, val) in map.iter_mut() {
                    if k != "UserData" && (val.is_object() || val.is_array()) {
                        self.apply(val, user);
                    }
                }
            }
            Value::Array(a) => a.iter_mut().for_each(|e| self.apply(e, user)),
            _ => {}
        }
    }
}

impl ResponseRewriter for UserDataOverlay {
    fn name(&self) -> &'static str {
        "user_data"
    }
    fn applies(&self, p: &Payload<'_>) -> bool {
        self.svc.enabled() && matches!(p.body, Some(PayloadBody::Json(_)))
    }
    fn rewrite(&self, p: &mut Payload<'_>, cx: &OutCtx<'_>) {
        if cx.proxy_user_id.is_empty() {
            return;
        }
        if let Some(PayloadBody::Json(v)) = &mut p.body {
            self.apply(v, cx.proxy_user_id);
        }
    }
}
