//! Per-user lists from the index and proxy-held user data: `Resume`,
//! `NextUp`, and write-through of user data changes to upstream copies.

use super::indexed::{hydrate, index_outcome, indexed_items, is_live, scope_of, warm_snapshot};
use super::*;
use crate::index::query::Scope;
use crate::types::ProxyItemId;
use crate::userdata::nextup::{next_up, NextUpQuery};

fn bool_param(q: &[(String, String)], k: &str) -> Option<bool> {
    q_get(q, k).map(|v| v.eq_ignore_ascii_case("true") || v == "1")
}

/// `UserItems/Resume`: in-progress items, most recently played first.
pub async fn indexed_resume(st: &Shared, r: &Req, c: &Client) -> Option<Result<Response>> {
    if !st.userdata.enabled() {
        return None;
    }
    let mut q: Vec<(String, String)> = r
        .query
        .iter()
        .filter(|(k, _)| {
            !["userid", "filters", "sortby", "sortorder", "recursive"]
                .contains(&k.to_ascii_lowercase().as_str())
        })
        .cloned()
        .collect();
    q.push(("Filters".into(), "IsResumable".into()));
    q.push(("Recursive".into(), "true".into()));
    q.push(("SortBy".into(), "DatePlayed".into()));
    q.push(("SortOrder".into(), "Descending".into()));
    indexed_items(st, r, c, "resume", &q).await
}

/// `Shows/NextUp` from merged per-user state: one entry per canonical
/// series, so a show split across servers has a single next episode.
pub async fn indexed_nextup(st: &Shared, r: &Req, c: &Client) -> Option<Result<Response>> {
    if !st.userdata.enabled() {
        return None;
    }
    let endpoint = "nextup";
    let snap = warm_snapshot(st, endpoint)?;
    let q = &r.query;
    let mut nq = NextUpQuery {
        cutoff: q_get(q, "NextUpDateCutoff").map(String::from),
        disable_first_episode: bool_param(q, "DisableFirstEpisode").unwrap_or(false),
        skip_resumable: bool_param(q, "EnableResumable") == Some(false),
        ..Default::default()
    };
    let mut nothing = false;
    if let Some(s) = q_get(q, "SeriesId") {
        match scope_of(st, &snap, s) {
            Some(Scope::Item(i)) => nq.series = Some(i),
            _ => nothing = true,
        }
    }
    if let Some(p) = q_get(q, "ParentId") {
        match scope_of(st, &snap, p) {
            Some(Scope::Libraries(l)) => nq.scope = Some(Scope::Libraries(l)),
            Some(Scope::Item(i)) if nq.series.is_none() => nq.series = Some(i),
            Some(Scope::Item(_)) => {}
            _ => nothing = true,
        }
    }
    index_outcome(endpoint, "index");
    let _s = Stage::start("index_page");
    let user = c.user_id.as_str();
    let all = if nothing {
        vec![]
    } else {
        let mut all = next_up(
            &snap,
            &|s| is_live(st, s),
            &|i| st.userdata.fact(user, &snap, i),
            &nq,
        );
        st.parental.retain(user, &snap, &mut all);
        all
    };
    let start: usize = q_get(q, "StartIndex")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let limit: Option<usize> = q_get(q, "Limit").and_then(|s| s.parse().ok());
    let s = start.min(all.len());
    let e = limit.map_or(all.len(), |l| (s + l).min(all.len()));
    let items = hydrate(st, c, &snap, &all[s..e], q).await;
    Some(Ok(ok_json(dto::query_result(items, all.len(), start))))
}

fn clone_req(r: &Req) -> Req {
    Req {
        method: r.method.clone(),
        segs: r.segs.clone(),
        query: r.query.clone(),
        headers: r.headers.clone(),
        body: r.body.clone(),
        raw_path: r.raw_path.clone(),
    }
}

/// Servers holding a copy of `id` (all copies of a grouped item).
fn copy_servers(st: &Shared, id: &ProxyItemId) -> Vec<ServerId> {
    let mut out: Vec<ServerId> = vec![];
    if let Some(snap) = st.index.latest() {
        if let Some(it) = snap.item(id) {
            for c in &it.copies {
                let s = snap.copies[*c].server;
                if !out.contains(&s) {
                    out.push(s);
                }
            }
        }
    }
    if out.is_empty() {
        if let Some(Target::Item { upstream, .. }) = st.ids.decode(id) {
            out.push(upstream);
        }
    }
    out
}

/// Send a user data change on to every live server with a copy of `id`
/// (ids are translated per server). Failures are counted and logged.
pub async fn write_through(st: &Shared, r: &Req, c: &Client, id: &ProxyItemId) {
    if !st.userdata.config().write_through {
        return;
    }
    for s in copy_servers(st, id) {
        if !is_live(st, s) {
            continue;
        }
        let outcome = match forward(st, s, clone_req(r), c, None).await {
            Ok(resp) if resp.status().is_success() => "ok",
            Ok(resp) => {
                tracing::debug!(status = %resp.status(), "user data write-through refused");
                "error"
            }
            Err(e) => {
                tracing::debug!("user data write-through failed: {e:#}");
                "error"
            }
        };
        counter!(names::USERDATA_WRITE_THROUGH, "outcome" => outcome).increment(1);
    }
}
