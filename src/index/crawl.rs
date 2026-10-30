//! Reading a server's whole catalog into index entries.
//!
//! [`Crawler`] is the seam; [`ItemsCrawler`] pages through
//! `Items?ParentId=<library>&Recursive=true` for every library the
//! upstream user sees (so each entry knows its libraries), asking only for
//! the fields the index needs, plus the server's parental rating scores.

use super::entry::{Entry, CRAWL_FIELDS};
use crate::backend::MediaBackend;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Result of one crawl.
#[derive(Debug, Default)]
pub struct Crawled {
    pub entries: Vec<Entry>,
    pub ratings: HashMap<String, i64>,
    pub libraries: usize,
}

#[async_trait]
pub trait Crawler: Send + Sync {
    async fn crawl(&self, up: &Arc<dyn MediaBackend>) -> Result<Crawled, String>;
}

pub struct ItemsCrawler {
    pub page_size: usize,
}

async fn get(
    up: &Arc<dyn MediaBackend>,
    path: &[&str],
    q: &[(&str, String)],
) -> Result<Value, String> {
    let uc = up.session().await;
    let mut q: Vec<(String, String)> = q.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    let segs = crate::paths::adapt(
        path.iter().map(|s| s.to_string()).collect(),
        &mut q,
        uc.modern,
        uc.user_id.as_deref(),
    );
    let mut pq = segs.join("/");
    if !q.is_empty() {
        pq.push('?');
        pq.push_str(&crate::rewrite::encode_pairs(&q));
    }
    match up.get_json(&pq).await {
        Ok((s, v)) if s.is_success() => Ok(v),
        Ok((s, _)) => Err(format!("{}: HTTP {s}", segs.join("/"))),
        Err(e) => Err(e.to_string()),
    }
}

/// Rating name -> score from `Localization/ParentalRatings` (10.11+:
/// `RatingScore.score`, older: `Value`). Unscored names are left out.
pub fn parse_ratings(v: &Value) -> HashMap<String, i64> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let name = r.get("Name")?.as_str()?.to_string();
            let score = r
                .get("RatingScore")
                .and_then(|s| s.get("score"))
                .and_then(|s| s.as_i64())
                .or_else(|| r.get("Value").and_then(|x| x.as_i64()))?;
            Some((name, score))
        })
        .collect()
}

#[async_trait]
impl Crawler for ItemsCrawler {
    async fn crawl(&self, up: &Arc<dyn MediaBackend>) -> Result<Crawled, String> {
        up.ensure_login().await.map_err(|e| e.to_string())?;
        let uid = up.session().await.user_id.unwrap_or_default();
        let views = get(up, &["Users", &uid, "Views"], &[]).await?;
        let mut out = Crawled::default();
        let mut pos: HashMap<String, usize> = HashMap::new();
        for view in views
            .get("Items")
            .and_then(|i| i.as_array())
            .into_iter()
            .flatten()
        {
            let Some(lib) = view
                .get("Id")
                .and_then(|i| i.as_str())
                .and_then(crate::types::normalize_hex_id)
            else {
                continue;
            };
            out.libraries += 1;
            let mut start = 0usize;
            loop {
                let q = [
                    ("ParentId", lib.clone()),
                    ("Recursive", "true".into()),
                    ("Fields", CRAWL_FIELDS.into()),
                    ("EnableImages", "false".into()),
                    ("EnableUserData", "false".into()),
                    ("StartIndex", start.to_string()),
                    ("Limit", self.page_size.to_string()),
                ];
                let page = get(up, &["Users", &uid, "Items"], &q).await?;
                let items = page.get("Items").and_then(|i| i.as_array());
                let n = items.map_or(0, Vec::len);
                let mut fresh = 0;
                for it in items.into_iter().flatten() {
                    let Some(e) = Entry::from_dto(it, Some(&lib)) else {
                        continue;
                    };
                    match pos.get(&e.id) {
                        Some(i) => {
                            let had = out.entries[*i].libraries.len();
                            out.entries[*i].absorb(e);
                            fresh += (out.entries[*i].libraries.len() > had) as usize;
                        }
                        None => {
                            pos.insert(e.id.clone(), out.entries.len());
                            out.entries.push(e);
                            fresh += 1;
                        }
                    }
                }
                start += n;
                let total = page
                    .get("TotalRecordCount")
                    .and_then(|t| t.as_u64())
                    .map(|t| t as usize);
                // Stop on a short/empty page, at the total, or when a page
                // brings nothing new (a server ignoring StartIndex).
                if n < self.page_size || total.is_some_and(|t| start >= t) || fresh == 0 {
                    break;
                }
            }
        }
        match get(up, &["Localization", "ParentalRatings"], &[]).await {
            Ok(v) => out.ratings = parse_ratings(&v),
            Err(e) => tracing::debug!(upstream = %up.name(), "parental ratings: {e}"),
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ratings_parse_both_shapes() {
        let r = parse_ratings(&json!([
            {"Name":"Unrated"},
            {"Name":"PG","Value":10,"RatingScore":{"score":10,"subScore":0}},
            {"Name":"R","Value":17}
        ]));
        assert_eq!(r.len(), 2);
        assert_eq!(r["PG"], 10);
        assert_eq!(r["R"], 17);
    }
}
