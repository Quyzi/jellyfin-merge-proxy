//! Library lists answered from the catalog index ([`crate::index`]).
//!
//! The index decides *which* items match and in what order (exact
//! `TotalRecordCount`, stable deep paging, Jellyfin sort semantics, merged
//! copies and genres/people); the page's DTOs are then fetched from the
//! best live copy of each item (`Items?Ids=...`, with the client's
//! `Fields`/image options) and rewritten like any upstream response.
//! Every function returns `None` when the index cannot answer (cold, or a
//! query it does not express) and the caller falls back to the live
//! fan-out.

use super::fetch::fetch_dtos;
use super::*;
use crate::index::query::{IndexQuery, Scope};
use crate::index::snapshot::Snapshot;
use crate::types::ProxyItemId;

pub(super) fn index_outcome(endpoint: &'static str, outcome: &'static str) {
    counter!(names::INDEX_QUERIES, "endpoint" => endpoint, "outcome" => outcome).increment(1);
}

/// The snapshot to answer from, if the index is warm.
pub(super) fn warm_snapshot(st: &Shared, endpoint: &'static str) -> Option<Arc<Snapshot>> {
    let snap = st.index.snapshot(&st.cfg(), &st.backends()[..]);
    if snap.is_none() {
        index_outcome(endpoint, "cold");
    }
    snap
}

pub(super) fn is_live(st: &Shared, s: ServerId) -> bool {
    st.backend(s).is_some_and(|b| !b.is_down())
}

/// What a `ParentId`-like value points at, or `None` if the index cannot
/// scope a query by it.
pub(super) fn scope_of(st: &Shared, snap: &Snapshot, raw: &str) -> Option<Scope> {
    let pid = ProxyItemId::parse(raw)?;
    match st.ids.decode(&pid) {
        Some(Target::View(v)) => {
            let libs = v
                .members
                .iter()
                .map(|(s, id)| (*s, id.as_str().to_string()))
                .collect();
            return Some(Scope::Libraries(libs));
        }
        Some(Target::Item { upstream, id })
            if snap
                .libraries
                .contains(&(upstream, id.as_str().to_string())) =>
        {
            return Some(Scope::Libraries(vec![(upstream, id.into_string())]));
        }
        _ => {}
    }
    let i = snap.item_index(&pid)?;
    let e = snap.primary(&snap.items[i]);
    (e.is("Series") || e.is("Season")).then_some(Scope::Item(i))
}

/// Run a client query against the index: (all matches sorted, page range).
pub(super) fn run_query(
    st: &Shared,
    snap: &Snapshot,
    q: &[(String, String)],
    endpoint: &'static str,
    user: &str,
) -> Option<(IndexQuery, Vec<usize>)> {
    let parent = |raw: &str| scope_of(st, snap, raw);
    let with_user = st.userdata.enabled();
    let iq = match IndexQuery::parse(q, snap, &parent, with_user) {
        Ok(iq) => iq,
        Err(reason) => {
            tracing::debug!(endpoint, %reason, "index cannot answer; using live fan-out");
            index_outcome(endpoint, "unsupported");
            return None;
        }
    };
    let fact = |it: &crate::index::snapshot::Item| {
        let i = snap.item_index(&it.id)?;
        Some(st.userdata.fact(user, snap, i))
    };
    let mut all = iq.run(snap, &|s| is_live(st, s), &fact);
    st.parental.retain(user, snap, &mut all);
    Some((iq, all))
}

/// Fetch DTOs for `page` (item indexes) from the best live copy of each,
/// rewritten for the client, in order. Items gone upstream are skipped.
pub(super) async fn hydrate(
    st: &Shared,
    c: &Client,
    snap: &Snapshot,
    page: &[usize],
    client_q: &[(String, String)],
) -> Vec<Value> {
    let mut picks: Vec<Option<(ServerId, String)>> = Vec::with_capacity(page.len());
    for &i in page {
        let item = &snap.items[i];
        let preferred = match st.ids.decode(&item.id) {
            Some(Target::Item { upstream, id })
                if is_live(st, upstream)
                    && item.copies.iter().any(|c| {
                        snap.copies[*c].server == upstream
                            && snap.copies[*c].entry.id == id.as_str()
                    }) =>
            {
                Some((upstream, id.into_string()))
            }
            _ => None,
        };
        let pick = preferred.or_else(|| {
            item.copies
                .iter()
                .map(|c| &snap.copies[*c])
                .find(|c| is_live(st, c.server))
                .map(|c| (c.server, c.entry.id.clone()))
        });
        picks.push(pick);
    }
    let mut dtos = fetch_dtos(st, c, &picks, client_q).await;
    let mut out = Vec::with_capacity(page.len());
    for (pick, &i) in picks.into_iter().zip(page) {
        match pick.and_then(|p| dtos.remove(&p)) {
            Some(mut v) => {
                // The canonical id even if the item was hydrated elsewhere.
                v["Id"] = Value::String(snap.items[i].id.as_str().to_string());
                out.push(v);
            }
            None => tracing::debug!(item = %snap.items[i].id, "indexed item missing upstream"),
        }
    }
    out
}

/// `Items` (and the user-scoped variant) from the index.
pub async fn indexed_items(
    st: &Shared,
    r: &Req,
    c: &Client,
    endpoint: &'static str,
    q: &[(String, String)],
) -> Option<Result<Response>> {
    let snap = warm_snapshot(st, endpoint)?;
    let (iq, all) = run_query(st, &snap, q, endpoint, &c.user_id)?;
    index_outcome(endpoint, "index");
    let _s = Stage::start("index_page");
    let page = iq.page(&all);
    let items = hydrate(st, c, &snap, page, &r.query).await;
    counter!(names::MERGE_ITEMS_OUT, "endpoint" => endpoint).increment(items.len() as u64);
    Some(Ok(ok_json(dto::query_result(items, all.len(), iq.start))))
}

/// `Shows/{id}/Seasons` and `Shows/{id}/Episodes` from the index: the
/// series' seasons/episodes from every server it is on, merged.
pub async fn indexed_show_children(
    st: &Shared,
    r: &Req,
    c: &Client,
    episodes: bool,
) -> Option<Result<Response>> {
    let endpoint = if episodes { "episodes" } else { "seasons" };
    let series = r.segs.get(1)?.clone();
    let sid = crate::types::normalize_hex_id(&series).and_then(|n| ProxyItemId::parse(&n));
    if sid.is_some_and(|id| st.parental.blocks(&c.user_id, &id)) {
        return Some(Ok(err_resp(StatusCode::NOT_FOUND, "Item not found")));
    }
    let mut q: Vec<(String, String)> = r
        .query
        .iter()
        .filter(|(k, _)| !k.eq_ignore_ascii_case("UserId"))
        .cloned()
        .collect();
    if episodes {
        q.push(("SeriesId".into(), series));
        q.push(("Recursive".into(), "true".into()));
        q.push(("IncludeItemTypes".into(), "Episode".into()));
        if q_get(&q, "SortBy").is_none() {
            q.push(("SortBy".into(), "ParentIndexNumber,IndexNumber".into()));
        }
    } else {
        q.push(("ParentId".into(), series));
        q.push(("IncludeItemTypes".into(), "Season".into()));
        if q_get(&q, "SortBy").is_none() {
            q.push(("SortBy".into(), "IndexNumber".into()));
        }
    }
    indexed_items(st, r, c, endpoint, &q).await
}
