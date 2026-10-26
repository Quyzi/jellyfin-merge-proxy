use anyhow::Result;
use futures_util::future::join_all;
use metrics::{counter, histogram};
use serde_json::{json, Value};
use std::time::Instant;

use crate::backend::MediaBackend;
use crate::cache::{CacheKey, Lookup};
use crate::ids::{looks_like_id, normalize_hex_id, synthetic_id, MergedView, Target};
use crate::merge::{self, ViewGroup};
use crate::telemetry::{names, Stage};
use crate::types::{ServerId, UpstreamItemId};
use crate::Shared;

use super::auth::anon_client;
use super::forward::{out_ctx, path_with_query};
use super::list::upstream_json;
use super::Client;

async fn fetch_views(up: &dyn MediaBackend) -> Result<Vec<Value>, String> {
    let server = up.name().to_string();
    let start = Instant::now();
    up.ensure_login().await.map_err(|e| e.to_string())?;
    let uc = up.session().await;
    let Some(uid) = uc.user_id.clone() else {
        return Err("no upstream user id".into());
    };
    let mut q = vec![];
    let segs = crate::paths::adapt(vec!["UserViews".into()], &mut q, uc.modern, Some(&uid));
    let r = up.get_json(&path_with_query(&segs, &q)).await;
    histogram!(names::CACHE_REFRESH_DURATION, "cache" => "views", "server" => server)
        .record(start.elapsed().as_secs_f64());
    match r {
        Ok((s, v)) if s.is_success() => Ok(v
            .get("Items")
            .and_then(|i| i.as_array())
            .cloned()
            .unwrap_or_default()),
        Ok((s, _)) => Err(format!("HTTP {s}")),
        Err(e) => Err(e.to_string()),
    }
}

async fn raw_views(st: &Shared, force: bool) -> Vec<(ServerId, Vec<Value>)> {
    let _g = st.views_refresh.lock().await;
    let settings = st.catalog.settings();
    let set = st.backends();
    let futs = set.iter().map(|up| {
        let st = st.clone();
        async move {
            let key = CacheKey::new("views", up.name());
            let stale = match st.catalog.get(&key) {
                Lookup::Fresh(v) if !force => return items_of(up.id(), v),
                Lookup::Fresh(v) | Lookup::Stale(v) => Some(v),
                Lookup::Miss => None,
            };
            let res = fetch_views(up.as_ref()).await;
            let outcome = match &res {
                Ok(_) => "success",
                Err(_) => "error",
            };
            counter!(names::FANOUT_RESULTS, "endpoint" => "views", "server" => up.name().to_string(), "outcome" => outcome).increment(1);
            match res {
                Ok(items) => {
                    st.catalog.put(key, Value::Array(items.clone()));
                    Some((up.id(), items))
                }
                Err(e) => {
                    tracing::warn!(upstream = %up.name(), "views: {e}");
                    counter!(names::CACHE_REFRESH_FAILURES, "cache" => "views", "server" => up.name().to_string()).increment(1);
                    if settings.serve_stale_on_error {
                        stale.and_then(|v| items_of(up.id(), v))
                    } else {
                        None
                    }
                }
            }
        }
    });
    let out: Vec<_> = join_all(futs).await.into_iter().flatten().collect();
    if !out.is_empty() && out.len() < set.len() {
        counter!(names::FANOUT_PARTIAL, "endpoint" => "views").increment(1);
    }
    out
}

fn items_of(id: ServerId, v: Value) -> Option<(ServerId, Vec<Value>)> {
    match v {
        Value::Array(a) => Some((id, a)),
        _ => None,
    }
}

pub async fn refresh_catalog(st: &Shared) {
    raw_views(st, true).await;
}

pub async fn views(st: &Shared, c: &Client, force: bool) -> Vec<Value> {
    let raw = raw_views(st, force).await;
    let _stage = Stage::start("views_build");
    let groups = merge::strategy_for(&st.cfg().merge).group_views(raw);
    let merged_n = groups
        .iter()
        .filter(|g| matches!(g, ViewGroup::Merged { .. }))
        .count();
    metrics::gauge!(names::LIBRARY_GROUPS, "kind" => "merged").set(merged_n as f64);
    metrics::gauge!(names::LIBRARY_GROUPS, "kind" => "single")
        .set((groups.len() - merged_n) as f64);
    let mut out = vec![];
    let mut name_count = std::collections::HashMap::<String, usize>::new();
    for g in &groups {
        let n = match g {
            ViewGroup::Single(_, v) => v.get("Name").and_then(|n| n.as_str()).unwrap_or(""),
            ViewGroup::Merged { name, .. } => name.as_str(),
        };
        *name_count.entry(n.to_lowercase()).or_default() += 1;
    }
    for g in groups {
        match g {
            ViewGroup::Single(u, mut v) => {
                let Some(up) = st.backend(u) else { continue };
                let uc = up.session().await;
                st.rewriters
                    .rewrite_json(&mut v, &out_ctx(st, up.as_ref(), &uc, c));
                if st.cfg().merge.label_colliding_libraries {
                    let name = v
                        .get("Name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    if name_count.get(&name.to_lowercase()).copied().unwrap_or(0) > 1 {
                        v["Name"] = json!(format!("{name} ({})", up.name()));
                    }
                }
                out.push(v);
            }
            ViewGroup::Merged {
                name,
                collection_type,
                members,
            } => {
                let id = synthetic_id(&[
                    "view",
                    &name.to_lowercase(),
                    &collection_type.clone().unwrap_or_default().to_lowercase(),
                ]);
                let (u0, first) = &members[0];
                let Some(up) = st.backend(*u0) else { continue };
                let uc = up.session().await;
                let mut t = first.clone();
                st.rewriters
                    .rewrite_json(&mut t, &out_ctx(st, up.as_ref(), &uc, c));
                t["Id"] = json!(id);
                t["DisplayPreferencesId"] = json!(id);
                t["Etag"] = json!(id);
                t["ParentId"] = json!(st.root_id);
                let member_ids = members
                    .iter()
                    .filter_map(|(u, v)| {
                        v.get("Id")
                            .and_then(|i| i.as_str())
                            .and_then(UpstreamItemId::parse)
                            .map(|i| (*u, i))
                    })
                    .collect();
                st.ids.register_view(MergedView {
                    id: id.clone(),
                    name,
                    collection_type,
                    members: member_ids,
                    template: t.clone(),
                });
                out.push(t);
            }
        }
    }
    out
}

pub async fn resolve_or_refresh(st: &Shared, id: &str) -> Option<Target> {
    if let Some(t) = st.ids.resolve(id) {
        return Some(t);
    }
    if !looks_like_id(id) {
        return None;
    }
    views(st, &anon_client(st), true).await;
    st.ids.resolve(id)
}

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
    let keys = crate::merge::provider_keys(&item);
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
            let theirs = crate::merge::provider_keys(it);
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
