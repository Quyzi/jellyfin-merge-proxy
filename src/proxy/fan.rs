//! Fan-out: querying every target upstream and merging the results.

use super::*;

pub(super) fn fan_outcome(e: &BackendError) -> &'static str {
    if e.is_timeout() {
        "timeout"
    } else {
        "error"
    }
}

#[tracing::instrument(level = "debug", skip_all, fields(endpoint = spec.name))]
pub async fn fan_list(st: &Shared, r: &Req, c: &Client, spec: &ListSpec) -> Result<Response> {
    let (targets, siblings) = list_targets(st, &r.query).await;
    fan_list_on(st, r, c, spec, targets, siblings).await
}

/// [`fan_list`] over explicit targets. A `{parent}` segment in `spec.path`
/// is replaced by each target's parent id. `siblings`: the targets are copies
/// of one series/season, so seasons and episodes are merged by number.
pub async fn fan_list_on(
    st: &Shared,
    r: &Req,
    c: &Client,
    spec: &ListSpec,
    targets: ListTargets,
    siblings: bool,
) -> Result<Response> {
    let cfg = st.cfg();
    let start: usize = q_get(&r.query, "StartIndex")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let limit: Option<usize> = q_get(&r.query, "Limit").and_then(|s| s.parse().ok());
    let fanout = targets.len() > 1;
    let endpoint = spec.name;
    let mut queried = 0usize;
    let mut futs = vec![];
    let user = c.user_id.as_str();
    for (u, parent) in targets {
        let Some(up) = st.backend(u) else {
            continue;
        };
        let Some(q) = query_for(
            st,
            &r.query,
            u,
            parent.as_ref().map(|p| p.as_str()),
            fanout.then_some((start, limit)),
        ) else {
            // Filter ids live only on other upstreams: not an error.
            counter!(names::FANOUT_RESULTS, "endpoint" => endpoint, "server" => up.name().to_string(), "outcome" => "skipped").increment(1);
            continue;
        };
        let mut q = q;
        if fanout && spec.dedupe_items && cfg.merge.dedupe && spec.shape != Shape::SearchHints {
            // Real servers only return ProviderIds when asked; dedupe needs them.
            ensure_field(&mut q, "ProviderIds");
        }
        queried += 1;
        let path: Vec<String> = spec
            .path
            .iter()
            .map(|s| match (s.as_str(), &parent) {
                ("{parent}", Some(p)) => p.to_string(),
                _ => s.clone(),
            })
            .collect();
        futs.push(async move {
            let outcome = |o: &'static str| {
                counter!(names::FANOUT_RESULTS, "endpoint" => endpoint, "server" => up.name().to_string(), "outcome" => o).increment(1);
            };
            let uc = match up.login_as(Some(user)).await {
                Ok(uc) => uc,
                Err(e) => {
                    tracing::warn!(upstream = %up.name(), "skipping: {e}");
                    outcome("skipped");
                    return Err(());
                }
            };
            let mut q = q;
            let segs = crate::paths::adapt(path, &mut q, uc.modern, uc.user_id.as_deref());
            match up.get_json_as(Some(user), &path_with_query(&segs, &q)).await {
                Ok((s, v)) if s.is_success() => {
                    outcome("success");
                    Ok((up, uc, v))
                }
                Ok((s, _)) => {
                    tracing::warn!(upstream = %up.name(), endpoint, "list: HTTP {s}");
                    outcome("http_error");
                    Err(())
                }
                Err(e) => {
                    tracing::warn!(upstream = %up.name(), endpoint, "list: {e}");
                    outcome(fan_outcome(&e));
                    Err(())
                }
            }
        });
    }
    histogram!(names::FANOUT_UPSTREAMS, "endpoint" => endpoint).record(queried as f64);
    let fan_stage = Stage::start("fanout");
    let wait = Instant::now();
    let raw = join_all(futs).await;
    histogram!(names::FANOUT_SLOWEST, "endpoint" => endpoint).record(wait.elapsed().as_secs_f64());
    drop(fan_stage);
    let results: Vec<(Arc<dyn MediaBackend>, SessionInfo, Value)> =
        raw.into_iter().filter_map(|r| r.ok()).collect();
    if !results.is_empty() && results.len() < queried {
        counter!(names::FANOUT_PARTIAL, "endpoint" => endpoint).increment(1);
    }

    let rw_stage = Stage::start("rewrite");
    let mut all: Vec<Ranked> = vec![];
    let mut total = 0usize;
    for (up, uc, mut v) in results {
        let ctx = out_ctx(st, up.as_ref(), &uc, c);
        let (items, t) = match spec.shape {
            Shape::QueryResult => (
                v.get_mut("Items").map(|i| i.take()),
                v.get("TotalRecordCount").and_then(|t| t.as_u64()),
            ),
            Shape::SearchHints => (
                v.get_mut("SearchHints").map(|i| i.take()),
                v.get("TotalRecordCount").and_then(|t| t.as_u64()),
            ),
            Shape::Array => (Some(v), None),
        };
        let items = items.and_then(|i| match i {
            Value::Array(a) => Some(a),
            _ => None,
        });
        let mut items = items.unwrap_or_default();
        let upstream_total = t.map(|t| t as usize).unwrap_or(items.len());
        for it in items.iter_mut() {
            st.rewriters.rewrite_json(it, &ctx);
        }
        let hidden = st.parental.retain_dtos(&c.user_id, &mut items);
        total += upstream_total.saturating_sub(hidden);
        let (priority, server) = (up.priority(), up.id());
        all.extend(items.into_iter().map(|item| Ranked {
            priority,
            server,
            item,
        }));
    }
    drop(rw_stage);
    counter!(names::MERGE_ITEMS_IN, "endpoint" => endpoint).increment(all.len() as u64);

    let mut removed = 0;
    let server_names = st.servers.names();
    if fanout && spec.dedupe_items && cfg.merge.dedupe {
        let _s = Stage::start("dedupe");
        let d = MeteredDeduplicator::new(
            merge::ProviderIdDeduplicator {
                types: cfg.merge.dedupe_types.clone(),
                // Search hints carry no ProviderIds: match on type+name+year.
                name_year_fallback: spec.shape == Shape::SearchHints,
            },
            &server_names,
        );
        let (a, stats) = d.dedupe(all);
        all = a;
        removed += stats.removed;
        link_siblings(st, &stats, &["Series"]);
    }
    if fanout && siblings && cfg.merge.dedupe {
        let _s = Stage::start("dedupe");
        let d = MeteredDeduplicator::new(merge::ChildIndexDeduplicator, &server_names);
        let (a, stats) = d.dedupe(all);
        all = a;
        removed += stats.removed;
        link_siblings(st, &stats, &["Season"]);
    }
    if fanout {
        // Copies the index grouped carry the same canonical id by now.
        let (a, n) = dedupe_by_id(all);
        all = a;
        removed += n;
    }
    if fanout && spec.dedupe_by_name {
        let _s = Stage::start("dedupe");
        let d = MeteredDeduplicator::new(merge::NameDeduplicator, &server_names);
        let (a, stats) = d.dedupe(all);
        all = a;
        removed += stats.removed;
    }
    let mut items: Vec<Value> = all.into_iter().map(|r| r.item).collect();
    if fanout {
        let _s = Stage::start("sort_page");
        let mut sort = merge::parse_sort(q_get(&r.query, "SortBy"), q_get(&r.query, "SortOrder"));
        if sort.is_empty() {
            sort = spec
                .default_sort
                .iter()
                .map(|(f, d)| (f.to_string(), *d))
                .collect();
        }
        merge::sort_items(&mut items, &sort);
        let s = if spec.shape == Shape::Array { 0 } else { start };
        items = merge::page(items, s, limit);
    }
    counter!(names::MERGE_ITEMS_OUT, "endpoint" => endpoint).increment(items.len() as u64);
    let total = total.saturating_sub(removed);
    Ok(match spec.shape {
        Shape::QueryResult => ok_json(dto::query_result(items, total, start)),
        Shape::SearchHints => ok_json(json!({"SearchHints": items, "TotalRecordCount": total})),
        Shape::Array => ok_json(Value::Array(items)),
    })
}

/// Drop items whose `Id` was already seen, keeping the best-priority copy
/// (first in server order on ties). Returns the kept items and how many
/// were dropped.
pub(super) fn dedupe_by_id(all: Vec<Ranked>) -> (Vec<Ranked>, usize) {
    let mut best: std::collections::HashMap<String, usize> = Default::default();
    let mut keep = vec![true; all.len()];
    for (i, r) in all.iter().enumerate() {
        let Some(id) = r.item.get("Id").and_then(|v| v.as_str()) else {
            continue;
        };
        match best.get(id).copied() {
            Some(j) if r.priority < all[j].priority => {
                keep[j] = false;
                best.insert(id.to_string(), i);
            }
            Some(_) => keep[i] = false,
            None => {
                best.insert(id.to_string(), i);
            }
        }
    }
    let removed = keep.iter().filter(|k| !**k).count();
    if removed > 0 {
        counter!(names::DEDUPE_DROPPED, "key" => "canonical").increment(removed as u64);
    }
    let out = all
        .into_iter()
        .zip(keep)
        .filter_map(|(r, k)| k.then_some(r))
        .collect();
    (out, removed)
}

/// Fan out a request returning an object and merge the objects:
/// arrays are unioned, numbers summed (`sum`) or the first value kept.
pub async fn fan_merge_objects(st: &Shared, r: &Req, c: &Client, sum: bool) -> Result<Response> {
    let (targets, _) = list_targets(st, &r.query).await;
    let segs: Vec<String> = r.segs.clone();
    let futs = targets.into_iter().filter_map(|(u, parent)| {
        let q = query_for(st, &r.query, u, parent.as_ref().map(|p| p.as_str()), None)?;
        let up = st.backend(u)?.clone();
        let segs = segs.clone();
        let user = c.user_id.as_str();
        Some(async move {
            let uc = up.login_as(Some(user)).await.ok()?;
            let mut q = q;
            let segs = crate::paths::adapt(segs, &mut q, uc.modern, uc.user_id.as_deref());
            match up
                .get_json_as(Some(user), &path_with_query(&segs, &q))
                .await
            {
                Ok((s, v)) if s.is_success() => Some((up, uc, v)),
                _ => None,
            }
        })
    });
    let mut acc = serde_json::Map::new();
    for (up, uc, mut v) in join_all(futs).await.into_iter().flatten() {
        st.rewriters
            .rewrite_json(&mut v, &out_ctx(st, up.as_ref(), &uc, c));
        let Value::Object(o) = v else { continue };
        for (k, val) in o {
            match (acc.get_mut(&k), val) {
                (None, val) => {
                    acc.insert(k, val);
                }
                (Some(Value::Array(a)), Value::Array(b)) => {
                    for x in b {
                        let key = x.get("Name").cloned().unwrap_or_else(|| x.clone());
                        if !a
                            .iter()
                            .any(|y| y.get("Name").cloned().unwrap_or_else(|| y.clone()) == key)
                        {
                            a.push(x);
                        }
                    }
                }
                (Some(Value::Number(a)), Value::Number(b)) if sum => {
                    *a = (a.as_i64().unwrap_or(0) + b.as_i64().unwrap_or(0)).into();
                }
                _ => {}
            }
        }
    }
    Ok(ok_json(Value::Object(acc)))
}
