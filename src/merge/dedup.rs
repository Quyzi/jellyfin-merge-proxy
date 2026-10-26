//! Deduplication of merged item lists.

use crate::types::ServerId;
use serde_json::Value;
use std::collections::HashMap;

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

pub fn provider_keys(item: &Value) -> Vec<String> {
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

#[derive(Debug, Clone)]
pub struct Ranked {
    pub priority: i64,
    pub server: ServerId,
    pub item: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedDuplicate {
    pub key: &'static str,
    pub winner: ServerId,
    pub winner_id: Option<String>,
    pub loser_id: Option<String>,
    pub item_type: Option<String>,
}

#[derive(Debug, Default)]
pub struct DedupeStats {
    pub removed: usize,
    pub drops: Vec<DroppedDuplicate>,
}

pub trait Deduplicator: Send + Sync {
    fn name(&self) -> &'static str;
    fn dedupe(&self, items: Vec<Ranked>) -> (Vec<Ranked>, DedupeStats);
}

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
    } else if k.contains(":index:") {
        "index"
    } else if k.contains(":tmdb:") {
        "tmdb"
    } else if k.contains(":imdb:") {
        "imdb"
    } else {
        "tvdb"
    }
}

#[derive(Debug, Clone)]
pub struct ProviderIdDeduplicator {
    pub types: Vec<String>,
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
        dedupe_by_keys(items, |v| if applies(v) { self.keys(v) } else { vec![] })
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ChildIndexDeduplicator;

fn child_index_key(v: &Value) -> Option<String> {
    let n = |k: &str| v.get(k).and_then(|x| x.as_i64());
    match v.get("Type").and_then(|t| t.as_str())? {
        "Season" => Some(format!("Season:index:{}", n("IndexNumber")?)),
        "Episode" => Some(format!(
            "Episode:index:{}:{}:{}",
            n("ParentIndexNumber")?,
            n("IndexNumber")?,
            n("IndexNumberEnd").unwrap_or(-1)
        )),
        _ => None,
    }
}

impl Deduplicator for ChildIndexDeduplicator {
    fn name(&self) -> &'static str {
        "child_index"
    }
    fn dedupe(&self, items: Vec<Ranked>) -> (Vec<Ranked>, DedupeStats) {
        dedupe_by_keys(items, |v| child_index_key(v).into_iter().collect())
    }
}

fn item_id(v: &Value) -> Option<String> {
    v.get("Id").and_then(|i| i.as_str()).map(String::from)
}

fn dedupe_by_keys(
    items: Vec<Ranked>,
    keys_of: impl Fn(&Value) -> Vec<String>,
) -> (Vec<Ranked>, DedupeStats) {
    let mut stats = DedupeStats::default();
    let mut winner: HashMap<String, usize> = HashMap::new();
    let mut alive = vec![true; items.len()];
    let drop = |kind: &'static str, w: &Ranked, l: &Ranked| DroppedDuplicate {
        key: kind,
        winner: w.server,
        winner_id: item_id(&w.item),
        loser_id: item_id(&l.item),
        item_type: l
            .item
            .get("Type")
            .and_then(|t| t.as_str())
            .map(String::from),
    };
    for (i, r) in items.iter().enumerate() {
        let keys = keys_of(&r.item);
        if keys.is_empty() {
            continue;
        }
        let existing = keys
            .iter()
            .find_map(|k| winner.get(k).map(|j| (*j, key_kind(k))));
        match existing {
            Some((j, kind)) if alive[j] => {
                if r.priority < items[j].priority {
                    alive[j] = false;
                    stats.drops.push(drop(kind, r, &items[j]));
                    for k in keys_of(&items[j].item).into_iter().chain(keys.clone()) {
                        winner.insert(k, i);
                    }
                } else {
                    alive[i] = false;
                    stats.drops.push(drop(kind, &items[j], r));
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
                        winner_id: None,
                        loser_id: item_id(&r.item),
                        item_type: None,
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
