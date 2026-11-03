//! Fetching item DTOs by upstream id (`Items?Ids=...`) on the requesting
//! user's account, rewritten for the client. Used to hydrate pages the
//! index decided (items, and genre/studio/person names).

use super::*;
use std::collections::HashMap;

/// Query parameters forwarded when fetching page DTOs.
pub(super) const DTO_PARAMS: &[&str] = &[
    "Fields",
    "EnableImages",
    "ImageTypeLimit",
    "EnableImageTypes",
    "EnableUserData",
];

/// The rewritten DTO of every `(server, upstream id)` in `picks` that the
/// server returned, keyed by the pick.
pub(crate) async fn fetch_dtos(
    st: &Shared,
    c: &Client,
    picks: &[Option<(ServerId, String)>],
    client_q: &[(String, String)],
) -> HashMap<(ServerId, String), Value> {
    let mut by_server: HashMap<ServerId, Vec<String>> = HashMap::new();
    for (s, id) in picks.iter().flatten() {
        by_server.entry(*s).or_default().push(id.clone());
    }
    let mut extra: Vec<(String, String)> = DTO_PARAMS
        .iter()
        .filter_map(|k| q_get(client_q, k).map(|v| (k.to_string(), v.to_string())))
        .collect();
    client_fields::apply(c, &["Items".to_string()], &mut extra);
    let user = c.user_id.as_str();
    let futs = by_server.into_iter().filter_map(|(s, ids)| {
        let up = st.backend(s)?;
        let extra = extra.clone();
        Some(async move {
            let mut got: Vec<(String, Value)> = vec![];
            let Ok(uc) = up.login_as(Some(user)).await else {
                return (up, None, got);
            };
            for chunk in ids.chunks(100) {
                let mut q = vec![("Ids".to_string(), chunk.join(","))];
                q.extend(extra.iter().cloned());
                let path = vec![
                    "Users".to_string(),
                    uc.user_id.clone().unwrap_or_default(),
                    "Items".to_string(),
                ];
                let segs = crate::paths::adapt(path, &mut q, uc.modern, uc.user_id.as_deref());
                match up
                    .get_json_as(Some(user), &path_with_query(&segs, &q))
                    .await
                {
                    Ok((s, mut v)) if s.is_success() => {
                        if let Some(Value::Array(a)) = v.get_mut("Items").map(Value::take) {
                            for it in a {
                                if let Some(id) = it.get("Id").and_then(|i| i.as_str()) {
                                    if let Some(id) = normalize_hex_id(id) {
                                        got.push((id, it));
                                    }
                                }
                            }
                        }
                    }
                    Ok((s, _)) => tracing::warn!(upstream = %up.name(), "index page: HTTP {s}"),
                    Err(e) => tracing::warn!(upstream = %up.name(), "index page: {e}"),
                }
            }
            (up, Some(uc), got)
        })
    });
    let mut dtos: HashMap<(ServerId, String), Value> = HashMap::new();
    for (up, uc, got) in join_all(futs).await {
        let Some(uc) = uc else { continue };
        let ctx = out_ctx(st, up.as_ref(), &uc, c);
        for (id, mut v) in got {
            st.rewriters.rewrite_json(&mut v, &ctx);
            dtos.insert((up.id(), id), v);
        }
    }
    dtos
}
