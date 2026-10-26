//! Merging, sorting, de-duplication and re-paging of item lists.

use serde_json::Value;
use std::cmp::Ordering;
use std::collections::HashMap;

use crate::types::ServerId;

/// (field, descending)
pub type SortSpec = Vec<(String, bool)>;

pub fn parse_sort(sort_by: Option<&str>, sort_order: Option<&str>) -> SortSpec {
    let Some(sb) = sort_by.filter(|s| !s.trim().is_empty()) else {
        return vec![];
    };
    let orders: Vec<bool> = sort_order
        .unwrap_or("")
        .split(',')
        .map(|o| o.trim().eq_ignore_ascii_case("descending"))
        .collect();
    sb.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .enumerate()
        .map(|(i, f)| {
            let desc = orders
                .get(i)
                .copied()
                .unwrap_or_else(|| orders.last().copied().unwrap_or(false));
            (f, desc)
        })
        .collect()
}

fn get_path<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cur = v;
    for p in path {
        cur = cur.get(*p)?;
    }
    if cur.is_null() {
        None
    } else {
        Some(cur)
    }
}

fn sort_value<'a>(item: &'a Value, field: &str) -> Option<&'a Value> {
    let f = field.to_ascii_lowercase();
    match f.as_str() {
        "sortname" => get_path(item, &["SortName"]).or_else(|| get_path(item, &["Name"])),
        "seriessortname" => get_path(item, &["SeriesName"]),
        "runtime" => get_path(item, &["RunTimeTicks"]),
        "dateplayed" => get_path(item, &["UserData", "LastPlayedDate"]),
        "playcount" => get_path(item, &["UserData", "PlayCount"]),
        "isplayed" => get_path(item, &["UserData", "Played"]),
        "isfavoriteorliked" => get_path(item, &["UserData", "IsFavorite"]),
        "artist" => get_path(item, &["Artists", "0"]).or_else(|| {
            item.get("Artists")
                .and_then(|a| a.get(0))
                .filter(|v| !v.is_null())
        }),
        "datelastcontentadded" => {
            get_path(item, &["DateLastMediaAdded"]).or_else(|| get_path(item, &["DateCreated"]))
        }
        _ => {
            // Case-insensitive direct field lookup.
            item.as_object().and_then(|o| {
                o.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(field))
                    .map(|(_, v)| v)
                    .filter(|v| !v.is_null())
            })
        }
    }
}

fn cmp_values(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&y.as_f64().unwrap_or(0.0))
            .unwrap_or(Ordering::Equal),
        (Value::String(x), Value::String(y)) => natural_cmp(&x.to_lowercase(), &y.to_lowercase()),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => a.to_string().cmp(&b.to_string()),
    }
}

/// Compare strings, treating runs of digits as numbers ("Ep 2" < "Ep 10").
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = ai.peek().copied().filter(|c| c.is_ascii_digit()) {
                    na.push(c);
                    ai.next();
                }
                let mut nb = String::new();
                while let Some(c) = bi.peek().copied().filter(|c| c.is_ascii_digit()) {
                    nb.push(c);
                    bi.next();
                }
                let na = na.trim_start_matches('0');
                let nb = nb.trim_start_matches('0');
                let o = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

/// Stable multi-key sort. Missing values sort last. "Random" shuffles.
pub fn sort_items(items: &mut [Value], spec: &SortSpec) {
    if spec.is_empty() {
        return;
    }
    if spec.iter().any(|(f, _)| f.eq_ignore_ascii_case("random")) {
        use rand::seq::SliceRandom;
        items.shuffle(&mut rand::thread_rng());
        return;
    }
    items.sort_by(|a, b| {
        for (field, desc) in spec {
            let o = match (sort_value(a, field), sort_value(b, field)) {
                (Some(x), Some(y)) => {
                    let o = cmp_values(x, y);
                    if *desc {
                        o.reverse()
                    } else {
                        o
                    }
                }
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            };
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    });
}

pub fn page(items: Vec<Value>, start: usize, limit: Option<usize>) -> Vec<Value> {
    let it = items.into_iter().skip(start);
    match limit {
        Some(l) => it.take(l).collect(),
        None => it.collect(),
    }
}

/// `Type:title:<lowercased name>:<year>`, for items without provider ids.
fn name_year_key(item: &Value) -> Option<String> {
    let ty = item.get("Type").and_then(|t| t.as_str()).unwrap_or("");
    let name = item
        .get("Name")
        .and_then(|t| t.as_str())?
        .trim()
        .to_lowercase();
    let year = item.get("ProductionYear").and_then(|y| y.as_i64())?;
    (!name.is_empty()).then(|| format!("{ty}:title:{name}:{year}"))
}

fn provider_keys(item: &Value) -> Vec<String> {
    let ty = item.get("Type").and_then(|t| t.as_str()).unwrap_or("");
    let mut keys = vec![];
    if let Some(p) = item.get("ProviderIds").and_then(|p| p.as_object()) {
        for (k, v) in p {
            let kl = k.to_ascii_lowercase();
            if matches!(kl.as_str(), "tmdb" | "imdb" | "tvdb") {
                if let Some(s) = v.as_str().filter(|s| !s.is_empty()) {
                    keys.push(format!("{ty}:{kl}:{s}"));
                }
            }
        }
    }
    keys
}

/// One item in a merged list, tagged with where it came from.
#[derive(Debug, Clone)]
pub struct Ranked {
    /// Lower wins (from `[[server]] priority`, default = position).
    pub priority: i64,
    pub server: ServerId,
    pub item: Value,
}

/// One duplicate removed: which key matched and whose copy was kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedDuplicate {
    pub key: &'static str,
    pub winner: ServerId,
}

/// What a [`Deduplicator`] did, for metrics.
#[derive(Debug, Default)]
pub struct DedupeStats {
    pub removed: usize,
    pub drops: Vec<DroppedDuplicate>,
}

/// Removes duplicates from a merged list.
pub trait Deduplicator: Send + Sync {
    fn name(&self) -> &'static str;
    /// Returns survivors (relative order kept unless documented) and stats.
    fn dedupe(&self, items: Vec<Ranked>) -> (Vec<Ranked>, DedupeStats);
}

/// No-op deduplicator.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoDedupe;

impl Deduplicator for NoDedupe {
    fn name(&self) -> &'static str {
        "none"
    }
    fn dedupe(&self, items: Vec<Ranked>) -> (Vec<Ranked>, DedupeStats) {
        (items, DedupeStats::default())
    }
}

fn key_kind(k: &str) -> &'static str {
    if k.contains(":title:") {
        "title"
    } else if k.contains(":tmdb:") {
        "tmdb"
    } else if k.contains(":imdb:") {
        "imdb"
    } else {
        "tvdb"
    }
}

/// Same item on several servers, matched by TMDB/IMDb/TVDB provider id,
/// for the configured item types. Among duplicates the lowest priority value
/// wins. Order of survivors is preserved.
#[derive(Debug, Clone)]
pub struct ProviderIdDeduplicator {
    pub types: Vec<String>,
    /// Also match items that have no `ProviderIds` at all by type, name and
    /// production year (search hints never carry provider ids).
    pub name_year_fallback: bool,
}

impl ProviderIdDeduplicator {
    fn keys(&self, item: &Value) -> Vec<String> {
        let mut keys = provider_keys(item);
        if keys.is_empty() && self.name_year_fallback && item.get("ProviderIds").is_none() {
            keys.extend(name_year_key(item));
        }
        keys
    }
}

impl Deduplicator for ProviderIdDeduplicator {
    fn name(&self) -> &'static str {
        "provider_ids"
    }
    fn dedupe(&self, items: Vec<Ranked>) -> (Vec<Ranked>, DedupeStats) {
        let applies = |v: &Value| {
            v.get("Type")
                .and_then(|t| t.as_str())
                .map(|t| self.types.iter().any(|x| x.eq_ignore_ascii_case(t)))
                .unwrap_or(false)
        };
        let mut stats = DedupeStats::default();
        let mut winner: HashMap<String, usize> = HashMap::new();
        let mut alive = vec![true; items.len()];
        for (i, r) in items.iter().enumerate() {
            if !applies(&r.item) {
                continue;
            }
            let keys = self.keys(&r.item);
            let existing = keys
                .iter()
                .find_map(|k| winner.get(k).map(|j| (*j, key_kind(k))));
            match existing {
                Some((j, kind)) if alive[j] => {
                    if r.priority < items[j].priority {
                        alive[j] = false;
                        stats.drops.push(DroppedDuplicate {
                            key: kind,
                            winner: r.server,
                        });
                        for k in self.keys(&items[j].item).into_iter().chain(keys.clone()) {
                            winner.insert(k, i);
                        }
                    } else {
                        alive[i] = false;
                        stats.drops.push(DroppedDuplicate {
                            key: kind,
                            winner: items[j].server,
                        });
                        for k in keys {
                            winner.entry(k).or_insert(j);
                        }
                    }
                }
                _ => {
                    for k in keys {
                        winner.insert(k, i);
                    }
                }
            }
        }
        stats.removed = alive.iter().filter(|a| !**a).count();
        let out = items
            .into_iter()
            .zip(alive)
            .filter_map(|(it, a)| a.then_some(it))
            .collect();
        (out, stats)
    }
}

/// Keeps one entry per (case-insensitive) `Name`, preferring the lowest
/// priority. Used for genre/studio/person lists. Output is priority-ordered.
#[derive(Debug, Default, Clone, Copy)]
pub struct NameDeduplicator;

impl Deduplicator for NameDeduplicator {
    fn name(&self) -> &'static str {
        "name"
    }
    fn dedupe(&self, mut items: Vec<Ranked>) -> (Vec<Ranked>, DedupeStats) {
        items.sort_by_key(|r| r.priority);
        let mut seen: HashMap<String, ServerId> = HashMap::new();
        let mut stats = DedupeStats::default();
        items.retain(|r| {
            let n = r
                .item
                .get("Name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_lowercase();
            match seen.get(&n) {
                Some(w) => {
                    stats.drops.push(DroppedDuplicate {
                        key: "name",
                        winner: *w,
                    });
                    false
                }
                None => {
                    seen.insert(n, r.server);
                    true
                }
            }
        });
        stats.removed = stats.drops.len();
        (items, stats)
    }
}

/// Decorator: counts `jmp_dedupe_dropped_total{key}` and
/// `jmp_dedupe_winner_total{server}` for any [`Deduplicator`].
pub struct MeteredDeduplicator<'a, D> {
    inner: D,
    server_names: &'a [String],
}

impl<'a, D: Deduplicator> MeteredDeduplicator<'a, D> {
    pub fn new(inner: D, server_names: &'a [String]) -> Self {
        MeteredDeduplicator {
            inner,
            server_names,
        }
    }
}

impl<D: Deduplicator> Deduplicator for MeteredDeduplicator<'_, D> {
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn dedupe(&self, items: Vec<Ranked>) -> (Vec<Ranked>, DedupeStats) {
        use crate::telemetry::names;
        let (out, stats) = self.inner.dedupe(items);
        for d in &stats.drops {
            metrics::counter!(names::DEDUPE_DROPPED, "key" => d.key).increment(1);
            if let Some(n) = self.server_names.get(d.winner.0) {
                metrics::counter!(names::DEDUPE_WINNER, "server" => n.clone()).increment(1);
            }
        }
        (out, stats)
    }
}

/// Result of grouping library views.
#[derive(Debug)]
pub enum ViewGroup {
    Single(ServerId, Value),
    Merged {
        name: String,
        collection_type: Option<String>,
        members: Vec<(ServerId, Value)>,
    },
}

/// Decides how libraries ("views") from several servers combine.
pub trait MergeStrategy: Send + Sync {
    fn name(&self) -> &'static str;
    fn group_views(&self, per_server: Vec<(ServerId, Vec<Value>)>) -> Vec<ViewGroup>;
}

/// Libraries with the same name and collection type become one view.
#[derive(Debug, Default, Clone, Copy)]
pub struct MergeByNameAndType;

/// Every library stays separate.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeepSeparate;

impl MergeStrategy for MergeByNameAndType {
    fn name(&self) -> &'static str {
        "by_name_and_type"
    }
    fn group_views(&self, per: Vec<(ServerId, Vec<Value>)>) -> Vec<ViewGroup> {
        group_views(per, true)
    }
}

impl MergeStrategy for KeepSeparate {
    fn name(&self) -> &'static str {
        "separate"
    }
    fn group_views(&self, per: Vec<(ServerId, Vec<Value>)>) -> Vec<ViewGroup> {
        group_views(per, false)
    }
}

/// The strategy selected by `[merge] merge_libraries_by_name`.
pub fn strategy_for(cfg: &crate::config::MergeConfig) -> &'static dyn MergeStrategy {
    if cfg.merge_libraries_by_name {
        &MergeByNameAndType
    } else {
        &KeepSeparate
    }
}

pub fn group_views(
    per_upstream: Vec<(ServerId, Vec<Value>)>,
    merge_by_name: bool,
) -> Vec<ViewGroup> {
    let mut order: Vec<(String, Option<String>)> = vec![];
    type Key = (String, Option<String>);
    let mut groups: HashMap<Key, Vec<(ServerId, Value)>> = HashMap::new();
    let mut singles: Vec<ViewGroup> = vec![];
    let mut out_order: Vec<Result<usize, (String, Option<String>)>> = vec![];
    for (u, views) in per_upstream {
        for v in views {
            if !merge_by_name {
                singles.push(ViewGroup::Single(u, v));
                out_order.push(Ok(singles.len() - 1));
                continue;
            }
            let name = v
                .get("Name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            let ct = v
                .get("CollectionType")
                .and_then(|n| n.as_str())
                .map(|s| s.to_ascii_lowercase());
            let key = (name.to_lowercase(), ct);
            if !groups.contains_key(&key) {
                order.push(key.clone());
                out_order.push(Err(key.clone()));
            }
            groups.entry(key).or_default().push((u, v));
        }
    }
    let mut singles: Vec<Option<ViewGroup>> = singles.into_iter().map(Some).collect();
    let mut result = vec![];
    for o in out_order {
        match o {
            Ok(i) => result.push(singles[i].take().unwrap()),
            Err(key) => {
                let mut members = groups.remove(&key).unwrap_or_default();
                if members.len() == 1 {
                    let (u, v) = members.pop().unwrap();
                    result.push(ViewGroup::Single(u, v));
                } else if !members.is_empty() {
                    let name = members[0]
                        .1
                        .get("Name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let collection_type = members[0]
                        .1
                        .get("CollectionType")
                        .and_then(|n| n.as_str())
                        .map(String::from);
                    result.push(ViewGroup::Merged {
                        name,
                        collection_type,
                        members,
                    });
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sort_and_page() {
        let mut items = vec![
            json!({"Name": "Ep 10", "PremiereDate": "2020-01-01"}),
            json!({"Name": "ep 2", "PremiereDate": "2021-01-01"}),
            json!({"Name": "Alpha"}),
            json!({"Name": "Ep 1", "SortName": "zzz"}),
        ];
        sort_items(&mut items, &parse_sort(Some("SortName"), Some("Ascending")));
        let names: Vec<_> = items.iter().map(|i| i["Name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["Alpha", "ep 2", "Ep 10", "Ep 1"]);
        sort_items(
            &mut items,
            &parse_sort(Some("PremiereDate,SortName"), Some("Descending")),
        );
        assert_eq!(items[0]["Name"], "ep 2");
        assert_eq!(items[1]["Name"], "Ep 10");
        let p = page(items, 1, Some(2));
        assert_eq!(p.len(), 2);
        assert_eq!(p[0]["Name"], "Ep 10");
    }

    #[test]
    fn dedupe_prefers_priority() {
        let r = |p: i64, s: usize, item: Value| Ranked {
            priority: p,
            server: ServerId(s),
            item,
        };
        let items = vec![
            r(
                1,
                1,
                json!({"Name": "M", "Type": "Movie", "ProviderIds": {"Tmdb": "1"}, "S": "b"}),
            ),
            r(
                0,
                0,
                json!({"Name": "M", "Type": "Movie", "ProviderIds": {"Tmdb": "1", "Imdb": "tt1"}, "S": "a"}),
            ),
            r(
                2,
                2,
                json!({"Name": "M", "Type": "Movie", "ProviderIds": {"Imdb": "tt1"}, "S": "c"}),
            ),
            r(
                2,
                2,
                json!({"Name": "Other", "Type": "Movie", "ProviderIds": {"Tmdb": "2"}}),
            ),
            r(
                2,
                2,
                json!({"Name": "Ep", "Type": "Episode", "ProviderIds": {"Tmdb": "1"}}),
            ),
        ];
        let names = vec!["a".to_string(), "b".into(), "c".into()];
        let d = MeteredDeduplicator::new(
            ProviderIdDeduplicator {
                types: vec!["Movie".to_string()],
                name_year_fallback: false,
            },
            &names,
        );
        let (out, stats) = d.dedupe(items.clone());
        assert_eq!(stats.removed, 2);
        let kinds: Vec<_> = stats.drops.iter().map(|d| d.key).collect();
        assert_eq!(kinds, vec!["tmdb", "imdb"]);
        assert!(
            stats.drops.iter().all(|d| d.winner == ServerId(0)),
            "winner is the priority-0 item"
        );
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].item["S"], "a");
        let (out, stats) = NoDedupe.dedupe(items.clone());
        assert_eq!((out.len(), stats.removed), (5, 0));
        let (out, stats) = NameDeduplicator.dedupe(items);
        assert_eq!(out.len(), 3);
        assert_eq!(stats.removed, 2);
        assert_eq!(out[0].item["S"], "a");
    }

    /// Regression (real Jellyfin 12.3 search hints): hints have no
    /// ProviderIds, so they dedupe by type + name + year when enabled, and
    /// items that have provider ids never fall back to the name.
    #[test]
    fn search_hints_dedupe_by_name_and_year() {
        let r = |p: i64, s: usize, v: Value| Ranked {
            priority: p,
            server: ServerId(s),
            item: v,
        };
        let items = vec![
            r(
                1,
                1,
                json!({"Name": "Big Buck Bunny", "Type": "Movie", "ProductionYear": 2008}),
            ),
            r(
                0,
                0,
                json!({"Name": "big buck bunny", "Type": "Movie", "ProductionYear": 2008}),
            ),
            r(
                2,
                2,
                json!({"Name": "Big Buck Bunny", "Type": "Movie", "ProductionYear": 2023}),
            ),
            r(
                2,
                2,
                json!({"Name": "Sintel", "Type": "Movie", "ProductionYear": 2010, "ProviderIds": {}}),
            ),
            r(
                0,
                0,
                json!({"Name": "Sintel", "Type": "Movie", "ProductionYear": 2010, "ProviderIds": {}}),
            ),
        ];
        let d = ProviderIdDeduplicator {
            types: vec!["Movie".into()],
            name_year_fallback: true,
        };
        let (out, stats) = d.dedupe(items.clone());
        assert_eq!(stats.removed, 1);
        assert_eq!(stats.drops[0].key, "title");
        assert_eq!(stats.drops[0].winner, ServerId(0));
        assert_eq!(out.len(), 4);
        let off = ProviderIdDeduplicator {
            types: vec!["Movie".into()],
            name_year_fallback: false,
        };
        assert_eq!(off.dedupe(items).1.removed, 0);
    }

    #[test]
    fn views_grouping() {
        let a = vec![
            json!({"Name": "Movies", "CollectionType": "movies"}),
            json!({"Name": "Music", "CollectionType": "music"}),
        ];
        let b = vec![
            json!({"Name": "movies", "CollectionType": "movies"}),
            json!({"Name": "Shows", "CollectionType": "tvshows"}),
        ];
        let (s0, s1) = (ServerId(0), ServerId(1));
        let g = MergeByNameAndType.group_views(vec![(s0, a.clone()), (s1, b.clone())]);
        assert_eq!(g.len(), 3);
        assert!(matches!(&g[0], ViewGroup::Merged { members, .. } if members.len() == 2));
        assert!(matches!(&g[1], ViewGroup::Single(ServerId(0), _)));
        assert!(matches!(&g[2], ViewGroup::Single(ServerId(1), _)));
        let g = KeepSeparate.group_views(vec![(s0, a), (s1, b)]);
        assert_eq!(g.len(), 4);
    }
}
