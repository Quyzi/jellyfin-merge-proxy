//! Sibling series: copies of one series/season on several upstreams.

use super::*;

/// Find copies of series `id` on the other servers (same TMDB/IMDb/TVDB id)
/// and record them as siblings. Runs once per id per process; only when
/// series dedupe is on. Lists that dedupe series link them too, this covers
/// a series opened before any merged list showed it (e.g. from Latest).
pub async fn discover_siblings(st: &Shared, id: &str) {
    let cfg = st.cfg();
    if !cfg.merge.dedupe
        || !cfg
            .merge
            .dedupe_types
            .iter()
            .any(|t| t.eq_ignore_ascii_case("Series"))
    {
        return;
    }
    let Some(pid) = normalize_hex_id(id).and_then(|n| crate::types::ProxyItemId::parse(&n)) else {
        return;
    };
    if st.siblings.group(&pid).is_some() || !st.siblings.first_check(&pid) {
        return;
    }
    let Some(Target::Item { upstream, id: uid }) = st.ids.decode(&pid) else {
        return;
    };
    let set = st.backends();
    let Some(owner) = set.iter().find(|b| b.id() == upstream).cloned() else {
        return;
    };
    let Some(item) = upstream_json(
        &owner,
        &["Items", uid.as_str()],
        &[("Fields", "ProviderIds")],
    )
    .await
    else {
        return;
    };
    if item.get("Type").and_then(|t| t.as_str()) != Some("Series") {
        return;
    }
    let keys = merge::provider_keys(&item);
    if keys.is_empty() {
        return;
    }
    let year = item
        .get("ProductionYear")
        .and_then(|y| y.as_i64())
        .map(|y| y.to_string());
    let name = item.get("Name").and_then(|n| n.as_str()).unwrap_or("");
    let mut q = vec![
        ("Recursive", "true"),
        ("IncludeItemTypes", "Series"),
        ("Fields", "ProviderIds"),
        ("Limit", "500"),
    ];
    match &year {
        Some(y) => q.push(("Years", y.as_str())),
        None => q.push(("SearchTerm", name)),
    }
    let mut found = vec![pid.clone()];
    for other in set.iter().filter(|b| b.id() != upstream && !b.is_down()) {
        let Some(v) = upstream_json(other, &["Items"], &q).await else {
            continue;
        };
        for it in v
            .get("Items")
            .and_then(|i| i.as_array())
            .into_iter()
            .flatten()
        {
            let theirs = merge::provider_keys(it);
            if theirs.iter().any(|k| keys.contains(k)) {
                if let Some(raw) = it
                    .get("Id")
                    .and_then(|i| i.as_str())
                    .and_then(UpstreamItemId::parse)
                {
                    found.push(st.ids.encode(other.id(), &raw).id);
                }
                break;
            }
        }
    }
    if found.len() > 1 {
        tracing::debug!(siblings = found.len() - 1, "series found on other servers");
        st.siblings.link(&found);
    }
}

/// Seasons / episodes of a series that has copies on several servers:
/// fanned out to every copy and merged by number. `None` if the series has
/// no siblings (the request is forwarded to its owner as usual).
pub async fn show_children(
    st: &Shared,
    r: &Req,
    c: &Client,
    spec: &ListSpec,
) -> Option<Result<Response>> {
    let id = r.segs.get(1)?;
    discover_siblings(st, id).await;
    let targets = with_siblings(st, id);
    if targets.len() < 2 {
        return None;
    }
    Some(fan_list_on(st, r, c, spec, targets, true).await)
}

/// Remember which dropped duplicates of `types` are copies of the kept item.
pub(super) fn link_siblings(st: &Shared, stats: &merge::DedupeStats, types: &[&str]) {
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

/// Make sure the upstream `Fields` list contains `field` (case-insensitive),
/// adding the parameter if the client sent none.
pub(crate) fn ensure_field(q: &mut Vec<(String, String)>, field: &str) {
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
