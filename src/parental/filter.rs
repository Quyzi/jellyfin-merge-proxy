//! [`ParentalFilter`]: drops items a restricted user may not see from
//! list responses that did not come from the index (fan-out Latest,
//! search hints, non-indexed lists).

use super::service::ParentalService;
use super::{allows_item, ContentPolicy, RatingPolicy};
use crate::index::snapshot::Snapshot;
use crate::rewrite::{OutCtx, Payload, PayloadBody, ResponseRewriter};
use crate::telemetry::names;
use crate::types::ProxyItemId;
use metrics::counter;
use serde_json::Value;
use std::sync::Arc;

pub struct ParentalFilter {
    svc: Arc<ParentalService>,
}

impl ParentalFilter {
    pub fn new(svc: Arc<ParentalService>) -> Self {
        ParentalFilter { svc }
    }
}

fn str_of<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// Whether one DTO passes: by its index entry when the index knows it,
/// else by `OfficialRating` (or its series' rating).
pub fn dto_allowed(p: &RatingPolicy, snap: Option<&Snapshot>, v: &Value) -> bool {
    if !v.is_object() {
        return true;
    }
    let kind = str_of(v, "Type").unwrap_or("");
    let idx = |k: &str| {
        let s = snap?;
        s.item_index(&ProxyItemId::parse(str_of(v, k)?)?)
    };
    if let (Some(s), Some(i)) = (snap, idx("Id").or_else(|| idx("ItemId"))) {
        return allows_item(p, s, i);
    }
    let Some(snap) = snap else {
        // No rating tables yet (index not built): fail closed for rated
        // video items, whose rating cannot be scored.
        let rated = str_of(v, "OfficialRating").is_some_and(|r| !r.is_empty());
        return !(rated && super::UNRATED_KINDS.contains(&kind)) && p.allows(None, kind);
    };
    let snap = Some(snap);
    let own = str_of(v, "OfficialRating")
        .and_then(|r| snap.and_then(|s| s.ratings.get(&r.to_lowercase()).copied()));
    let score = own.or_else(|| {
        let s = snap?;
        let i = idx("SeriesId")?;
        super::effective_score(s, i)
    });
    p.allows(score, kind)
}

/// Filter `arr` in place; returns how many were removed.
pub fn retain_page(p: &RatingPolicy, snap: Option<&Snapshot>, arr: &mut Vec<Value>) -> usize {
    let before = arr.len();
    arr.retain(|v| dto_allowed(p, snap, v));
    before - arr.len()
}

pub fn apply(p: &RatingPolicy, snap: Option<&Snapshot>, v: &mut Value) -> usize {
    match v {
        Value::Array(a) => retain_page(p, snap, a),
        Value::Object(m) => {
            let mut removed = 0;
            for key in ["Items", "SearchHints"] {
                if let Some(Value::Array(a)) = m.get_mut(key) {
                    removed += retain_page(p, snap, a);
                }
            }
            if removed > 0 {
                if let Some(t) = m.get("TotalRecordCount").and_then(Value::as_u64) {
                    let t = t.saturating_sub(removed as u64);
                    m.insert("TotalRecordCount".into(), Value::from(t));
                }
            }
            removed
        }
        _ => 0,
    }
}

impl ResponseRewriter for ParentalFilter {
    fn name(&self) -> &'static str {
        "parental"
    }
    fn applies(&self, p: &Payload<'_>) -> bool {
        matches!(p.body, Some(PayloadBody::Json(_)))
    }
    fn rewrite(&self, p: &mut Payload<'_>, cx: &OutCtx<'_>) {
        let Some((policy, snap)) = self.svc.current(cx.proxy_user_id) else {
            return;
        };
        if let Some(PayloadBody::Json(v)) = &mut p.body {
            let n = apply(&policy, snap.as_deref(), v);
            if n > 0 {
                counter!(names::PARENTAL_HIDDEN, "source" => "list").increment(n as u64);
            }
        }
    }
}
