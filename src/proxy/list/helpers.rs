use serde_json::Value;
use std::sync::Arc;

use crate::backend::MediaBackend;
use crate::error::BackendError;
use crate::ids::{normalize_hex_id, Target};
use crate::merge;
use crate::telemetry::Stage;
use crate::types::ServerId;
use crate::Shared;

use super::super::catalog::resolve_or_refresh;
use super::super::forward::path_with_query;
use super::super::response::{is_token_key, q_get};
use super::ListTargets;

pub fn with_siblings(st: &Shared, id: &str) -> ListTargets {
    let Some(pid) = crate::types::ProxyItemId::parse(&normalize_hex_id(id).unwrap_or_default())
    else {
        return vec![];
    };
    let mut ids = vec![pid.clone()];
    if let Some(g) = st.siblings.group(&pid) {
        ids.extend(g.iter().filter(|m| **m != pid).cloned());
    }
    let mut out: ListTargets = vec![];
    for m in ids {
        if let Some(Target::Item { upstream, id }) = st.ids.decode(&m) {
            let active = st.backend(upstream).is_some_and(|b| !b.is_down());
            if active && !out.iter().any(|(u, _)| *u == upstream) {
                out.push((upstream, Some(id)));
            }
        }
    }
    out
}

pub async fn list_targets(st: &Shared, q: &[(String, String)]) -> (ListTargets, bool) {
    let _s = Stage::start("id_decode");
    let parent = q_get(q, "ParentId")
        .filter(|p| normalize_hex_id(p).as_deref() != Some(st.root_id.as_str()));
    if let Some(p) = parent {
        return match resolve_or_refresh(st, p).await {
            Some(Target::View(v)) => (
                v.members
                    .iter()
                    .map(|(u, id)| (*u, Some(id.clone())))
                    .collect(),
                false,
            ),
            Some(Target::Item { upstream, id }) => {
                let t = with_siblings(st, p);
                if t.len() > 1 {
                    (t, true)
                } else {
                    (vec![(upstream, Some(id))], false)
                }
            }
            None => {
                tracing::debug!("unknown ParentId {p}");
                (vec![], false)
            }
        };
    }
    for key in ["SeriesId", "SeasonId", "AlbumId"] {
        if let Some(v) = q_get(q, key) {
            if let Some(Target::Item { upstream, .. }) = resolve_or_refresh(st, v).await {
                let t: ListTargets = with_siblings(st, v)
                    .into_iter()
                    .map(|(u, _)| (u, None))
                    .collect();
                if t.len() > 1 {
                    return (t, true);
                }
                return (vec![(upstream, None)], false);
            }
        }
    }
    let all = st
        .backends()
        .iter()
        .filter(|u| !u.is_down())
        .map(|u| (u.id(), None))
        .collect();
    (all, false)
}

pub fn query_for(
    st: &Shared,
    q: &[(String, String)],
    u: ServerId,
    parent: Option<&str>,
    fanout: Option<(usize, Option<usize>)>,
) -> Option<Vec<(String, String)>> {
    let mut out = vec![];
    for (k, v) in q {
        let kl = k.to_ascii_lowercase();
        if is_token_key(k) || kl == "userid" {
            continue;
        }
        if kl == "parentid" {
            if let Some(p) = parent {
                out.push((k.clone(), p.to_string()));
            }
            continue;
        }
        if fanout.is_some() && (kl == "startindex" || kl == "limit") {
            continue;
        }
        let parts: Vec<&str> = v.split(',').collect();
        let mut kept = vec![];
        let mut had_ids = false;
        for part in parts {
            match normalize_hex_id(part).and_then(|n| st.ids.resolve(&n)) {
                Some(Target::Item { upstream, id }) => {
                    had_ids = true;
                    if upstream == u {
                        kept.push(id.into_string());
                    } else if let Some((_, Some(sid))) =
                        with_siblings(st, part).into_iter().find(|(x, _)| *x == u)
                    {
                        kept.push(sid.into_string());
                    }
                }
                Some(Target::View(view)) => {
                    had_ids = true;
                    if let Some((_, id)) = view.members.iter().find(|(x, _)| *x == u) {
                        kept.push(id.to_string());
                    }
                }
                None => kept.push(part.to_string()),
            }
        }
        if had_ids && kept.is_empty() {
            return None;
        }
        out.push((k.clone(), kept.join(",")));
    }
    if let Some((start, limit)) = fanout {
        let want = match limit {
            Some(l) => (start + l).min(st.cfg().merge.fanout_limit),
            None => st.cfg().merge.fanout_limit,
        };
        out.push(("StartIndex".into(), "0".into()));
        out.push(("Limit".into(), want.to_string()));
    }
    Some(out)
}

pub async fn upstream_json(
    up: &Arc<dyn MediaBackend>,
    path: &[&str],
    query: &[(&str, &str)],
) -> Option<Value> {
    up.ensure_login().await.ok()?;
    let uc = up.session().await;
    let mut q: Vec<(String, String)> = query
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let segs = crate::paths::adapt(
        path.iter().map(|s| s.to_string()).collect(),
        &mut q,
        uc.modern,
        uc.user_id.as_deref(),
    );
    match up.get_json(&path_with_query(&segs, &q)).await {
        Ok((s, v)) if s.is_success() => Some(v),
        _ => None,
    }
}

pub fn link_siblings(st: &Shared, stats: &merge::DedupeStats, types: &[&str]) {
    for d in &stats.drops {
        let ty = d.item_type.as_deref().unwrap_or("");
        if !types.iter().any(|t| t.eq_ignore_ascii_case(ty)) {
            continue;
        }
        let ids: Vec<_> = [&d.winner_id, &d.loser_id]
            .into_iter()
            .filter_map(|i| i.as_deref())
            .filter_map(normalize_hex_id)
            .filter_map(|i| crate::types::ProxyItemId::parse(&i))
            .collect();
        if ids.len() == 2 {
            st.siblings.link(&ids);
        }
    }
}

pub fn ensure_field(q: &mut Vec<(String, String)>, field: &str) {
    if let Some((_, v)) = q.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case("fields")) {
        if !v.split(',').any(|f| f.trim().eq_ignore_ascii_case(field)) {
            if v.trim().is_empty() {
                *v = field.to_string();
            } else {
                v.push(',');
                v.push_str(field);
            }
        }
    } else {
        q.push(("Fields".into(), field.to_string()));
    }
}

pub fn fan_outcome(e: &BackendError) -> &'static str {
    if e.is_timeout() {
        "timeout"
    } else {
        "error"
    }
}
