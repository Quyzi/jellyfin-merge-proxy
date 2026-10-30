//! Fan-out list endpoints: specs, targets and per-upstream queries.

use super::*;

// ---------------------------------------------------------------------------
// Fan-out lists
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum Shape {
    QueryResult,
    Array,
    SearchHints,
}

/// How a fan-out list endpoint is queried and merged.
pub struct ListSpec {
    /// Metrics label.
    pub name: &'static str,
    /// Canonical (modern) upstream path.
    pub path: Vec<String>,
    pub shape: Shape,
    /// Sort when the client sends no SortBy (fan-out only).
    pub default_sort: &'static [(&'static str, bool)],
    pub dedupe_items: bool,
    pub dedupe_by_name: bool,
}

macro_rules! spec {
    ($name:ident, $label:expr, [$($seg:expr),*], $shape:expr, $sort:expr, $dd:expr, $dn:expr) => {
        pub static $name: std::sync::LazyLock<ListSpec> = std::sync::LazyLock::new(|| ListSpec {
            name: $label,
            path: vec![$($seg.to_string()),*],
            shape: $shape,
            default_sort: $sort,
            dedupe_items: $dd,
            dedupe_by_name: $dn,
        });
    };
}
spec!(
    ITEMS,
    "items",
    ["Items"],
    Shape::QueryResult,
    &[],
    true,
    false
);
spec!(
    LATEST,
    "latest",
    ["Items", "Latest"],
    Shape::Array,
    &[("DateCreated", true)],
    true,
    false
);
spec!(
    RESUME,
    "resume",
    ["UserItems", "Resume"],
    Shape::QueryResult,
    &[("DatePlayed", true)],
    false,
    false
);
spec!(
    NEXTUP,
    "nextup",
    ["Shows", "NextUp"],
    Shape::QueryResult,
    &[],
    false,
    false
);
spec!(
    SEARCH,
    "search",
    ["Search", "Hints"],
    Shape::SearchHints,
    &[],
    true,
    false
);
spec!(
    SEASONS,
    "seasons",
    ["Shows", "{parent}", "Seasons"],
    Shape::QueryResult,
    &[("IndexNumber", false)],
    false,
    false
);
spec!(
    EPISODES,
    "episodes",
    ["Shows", "{parent}", "Episodes"],
    Shape::QueryResult,
    &[("ParentIndexNumber", false), ("IndexNumber", false)],
    false,
    false
);
#[allow(non_upper_case_globals)]
pub const NAMED: ListSpec = ListSpec {
    name: "named",
    path: Vec::new(),
    shape: Shape::QueryResult,
    default_sort: &[("SortName", false)],
    dedupe_items: false,
    dedupe_by_name: true,
};

/// Targets of a list request: (upstream, upstream-side parent id).
pub type ListTargets = Vec<(ServerId, Option<UpstreamItemId>)>;

/// `(server, upstream id)` of `id` and of each of its siblings (copies of
/// the same series/season on other servers), owner first, active servers
/// only. A single entry if it has no siblings.
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

/// Upstreams (and the upstream-side parent id) a list request goes to, and
/// whether they are copies of one series/season (so children merge by number).
pub(super) async fn list_targets(st: &Shared, q: &[(String, String)]) -> (ListTargets, bool) {
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
    // Series / season scoped queries go to the owner (and its siblings).
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

/// Translate the client query for one upstream. Returns None if the query
/// filters on ids none of which live on this upstream.
pub(super) fn query_for(
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
                        // The same series/season on this server.
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

/// GET `path?query` on one upstream as the proxy's upstream user.
pub(super) async fn upstream_json(
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
