//! `Movies/Recommendations` and `Items/Suggestions` across servers.

use super::*;
use std::collections::HashMap;

/// `RecommendationType` in Jellyfin's enum order (the order its own
/// response is sorted by).
const TYPE_ORDER: &[&str] = &[
    "SimilarToRecentlyPlayed",
    "SimilarToLikedItem",
    "HasDirectorFromRecentlyPlayed",
    "HasActorFromRecentlyPlayed",
    "HasLikedDirector",
    "HasLikedActor",
];

fn type_rank(cat: &Value) -> usize {
    cat.get("RecommendationType")
        .and_then(|t| t.as_str())
        .and_then(|t| TYPE_ORDER.iter().position(|o| o.eq_ignore_ascii_case(t)))
        .unwrap_or(usize::MAX)
}

fn group_key(cat: &Value) -> (String, String) {
    let s = |k: &str| {
        cat.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_lowercase()
    };
    (s("RecommendationType"), s("BaselineItemName"))
}

/// Merge each server's recommendation categories: categories with the same
/// type and baseline item are one (items unioned by id, capped at
/// `item_limit`), ordered by type and capped at `category_limit`.
pub(super) fn merge_recommendations(
    per_server: Vec<Vec<Value>>,
    item_limit: usize,
    category_limit: usize,
) -> Vec<Value> {
    let mut groups: Vec<Value> = vec![];
    let mut at: HashMap<(String, String), usize> = HashMap::new();
    for cat in per_server.into_iter().flatten() {
        let key = group_key(&cat);
        match at.get(&key) {
            None => {
                at.insert(key, groups.len());
                groups.push(cat);
            }
            Some(i) => {
                let Some(Value::Array(extra)) = cat.get("Items").cloned() else {
                    continue;
                };
                let Some(Value::Array(have)) = groups[*i].get_mut("Items") else {
                    continue;
                };
                for it in extra {
                    if it.get("Id").is_none() || !have.iter().any(|h| h.get("Id") == it.get("Id")) {
                        have.push(it);
                    }
                }
            }
        }
    }
    for g in groups.iter_mut() {
        if let Some(Value::Array(items)) = g.get_mut("Items") {
            items.truncate(item_limit);
        }
    }
    groups.sort_by_key(type_rank);
    groups.truncate(category_limit);
    groups
}

pub async fn movie_recommendations(st: &Shared, r: &Req, c: &Client) -> Result<Response> {
    let (queried, raws) =
        fan_json(st, r, c, "recommendations", &["Movies", "Recommendations"]).await;
    if all_failed(st, queried, raws.len()) {
        return Ok(unavailable());
    }
    let limit = |key: &str, default: usize| {
        q_get(&r.query, key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    let per_server: Vec<Vec<Value>> = raws
        .iter()
        .map(|raw| {
            let Value::Array(cats) = raw.value.clone() else {
                return vec![];
            };
            // One category at a time: the parental filter and user-data
            // overlay act on an object's `Items`.
            cats.into_iter()
                .map(|mut cat| {
                    raw.rewrite(st, c, &mut cat);
                    cat
                })
                .collect()
        })
        .collect();
    let merged =
        merge_recommendations(per_server, limit("itemLimit", 8), limit("categoryLimit", 5));
    Ok(ok_json(Value::Array(merged)))
}

/// `Items/Suggestions`: a random merged page. Without
/// `EnableTotalRecordCount=true` the total is the page size, as on one server.
pub async fn suggestions(st: &Shared, r: &Req, c: &Client) -> Result<Response> {
    let resp = fan_list(st, r, c, &SUGGESTIONS).await?;
    if !resp.status().is_success()
        || q_get(&r.query, "EnableTotalRecordCount").is_some_and(|v| v.eq_ignore_ascii_case("true"))
    {
        return Ok(resp);
    }
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await?;
    let mut v: Value = serde_json::from_slice(&bytes)?;
    let n = v["Items"].as_array().map_or(0, Vec::len);
    v["TotalRecordCount"] = n.into();
    Ok(ok_json(v))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cat(t: &str, base: &str, ids: &[&str]) -> Value {
        json!({"RecommendationType": t, "BaselineItemName": base, "CategoryId": base,
               "Items": ids.iter().map(|i| json!({"Id": i})).collect::<Vec<_>>()})
    }

    /// Categories for the same baseline on two servers showed up twice (or
    /// the second server's items were lost); they must be one group with
    /// the union of items, ordered by Jellyfin's recommendation type order.
    #[test]
    fn same_baseline_merges_and_types_order_categories() {
        let a = vec![
            cat("HasLikedActor", "Zed", &["1"]),
            cat("SimilarToRecentlyPlayed", "Heat", &["1", "2"]),
        ];
        let b = vec![
            cat("SimilarToRecentlyPlayed", "heat", &["2", "3"]),
            cat("SimilarToLikedItem", "Alien", &["4"]),
        ];
        let m = merge_recommendations(vec![a, b], 8, 5);
        let types: Vec<&str> = m
            .iter()
            .map(|c| c["RecommendationType"].as_str().unwrap())
            .collect();
        assert_eq!(
            types,
            [
                "SimilarToRecentlyPlayed",
                "SimilarToLikedItem",
                "HasLikedActor"
            ]
        );
        let ids: Vec<&str> = m[0]["Items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["Id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["1", "2", "3"]);
    }

    /// `itemLimit` and `categoryLimit` bound the merged answer, not each
    /// server's.
    #[test]
    fn limits_apply_after_merging() {
        let a = vec![cat("SimilarToLikedItem", "a", &["1", "2", "3"])];
        let b = vec![
            cat("SimilarToLikedItem", "b", &["9"]),
            cat("HasLikedActor", "c", &["8"]),
        ];
        let m = merge_recommendations(vec![a, b], 2, 2);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0]["Items"].as_array().unwrap().len(), 2);
    }
}
