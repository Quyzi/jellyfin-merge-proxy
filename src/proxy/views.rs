//! Library views: fetching, caching and merging them.

use super::*;

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

/// Fetch one backend's views from upstream (no cache).
pub(super) async fn fetch_views(up: &dyn MediaBackend) -> Result<Vec<Value>, String> {
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

/// Per-backend views through the catalog cache: fresh entries are used
/// as-is; otherwise the backend is asked, falling back to a stale entry
/// (`cache.serve_stale_on_error`) when it fails.
pub(super) async fn raw_views(st: &Shared, force: bool) -> Vec<(ServerId, Vec<Value>)> {
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
    // A server without views (down, no stale copy) makes every merged
    // library silently partial: count it like any other partial fan-out.
    if !out.is_empty() && out.len() < set.len() {
        counter!(names::FANOUT_PARTIAL, "endpoint" => "views").increment(1);
    }
    out
}

pub(super) fn items_of(id: ServerId, v: Value) -> Option<(ServerId, Vec<Value>)> {
    match v {
        Value::Array(a) => Some((id, a)),
        _ => None,
    }
}

/// Refresh every backend's catalog entry now (background refresher).
pub async fn refresh_catalog(st: &Shared) {
    raw_views(st, true).await;
}

/// Build the merged, client-facing list of library views.
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

/// Resolve a proxy id; on a miss, refresh views once (merged view ids are
/// not persisted) and retry.
pub(super) async fn resolve_or_refresh(st: &Shared, id: &str) -> Option<Target> {
    if let Some(t) = st.ids.resolve(id) {
        return Some(t);
    }
    if !looks_like_id(id) {
        return None;
    }
    views(st, &anon_client(st), true).await;
    st.ids.resolve(id)
}
