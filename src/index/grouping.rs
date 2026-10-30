//! Which index entries are copies of the same thing.
//!
//! [`GroupingPolicy`] is the seam; [`ProviderGrouping`] is the default:
//! movies/series (the `[merge] dedupe_types`) match by Tmdb/Imdb/Tvdb id,
//! or by type + name + year when an entry has no provider ids at all.
//! When series are grouped, their seasons group by season number and their
//! episodes by season + episode number, so a show split across servers
//! becomes one show. Copies on the same server are never matched directly
//! (Jellyfin keeps them apart on purpose).
//!
//! [`assign_canonical`] picks each group's stable canonical id.

use super::entry::{normalize_name, Entry};
use crate::types::{ProxyItemId, ServerId};
use std::collections::{BTreeSet, HashMap};

/// One copy as the grouping sees it.
pub struct Copy<'a> {
    pub server: ServerId,
    pub entry: &'a Entry,
}

pub trait GroupingPolicy: Send + Sync {
    fn name(&self) -> &'static str;
    /// Partition `copies` (by index) into groups. Every index appears once.
    fn group(&self, copies: &[Copy<'_>]) -> Vec<Vec<usize>>;
}

/// Default policy (see the module docs). `enabled = false` keeps every
/// copy separate (`[merge] dedupe = false`).
#[derive(Debug, Clone)]
pub struct ProviderGrouping {
    pub enabled: bool,
    pub types: Vec<String>,
}

struct Uf(Vec<usize>);

impl Uf {
    fn find(&mut self, mut x: usize) -> usize {
        while self.0[x] != x {
            self.0[x] = self.0[self.0[x]];
            x = self.0[x];
        }
        x
    }
    fn union(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a.max(b)] = a.min(b);
        }
    }
}

/// Union the copies sharing a key, one copy per server per key.
fn union_keys(uf: &mut Uf, keyed: HashMap<String, Vec<(ServerId, usize)>>) {
    for (_, list) in keyed {
        let mut first: Option<usize> = None;
        let mut seen: BTreeSet<ServerId> = BTreeSet::new();
        for (srv, i) in list {
            if !seen.insert(srv) {
                continue;
            }
            match first {
                None => first = Some(i),
                Some(f) => uf.union(f, i),
            }
        }
    }
}

impl ProviderGrouping {
    fn groups_type(&self, kind: &str) -> bool {
        self.types.iter().any(|t| t.eq_ignore_ascii_case(kind))
    }
}

impl GroupingPolicy for ProviderGrouping {
    fn name(&self) -> &'static str {
        "provider_ids"
    }

    fn group(&self, copies: &[Copy<'_>]) -> Vec<Vec<usize>> {
        let mut uf = Uf((0..copies.len()).collect());
        if self.enabled {
            // 1. Top-level items by provider id (or name + year).
            let mut keyed: HashMap<String, Vec<(ServerId, usize)>> = HashMap::new();
            for (i, c) in copies.iter().enumerate() {
                let e = c.entry;
                if !self.groups_type(&e.kind) {
                    continue;
                }
                let kind = e.kind.to_ascii_lowercase();
                let mut keys: Vec<String> = ["tmdb", "imdb", "tvdb"]
                    .iter()
                    .filter_map(|p| e.providers.get(*p).map(|v| format!("{kind}:{p}:{v}")))
                    .collect();
                if keys.is_empty() && !e.name.is_empty() {
                    let y = e.year.map(|y| y.to_string()).unwrap_or_default();
                    keys.push(format!("{kind}:name:{}:{y}", normalize_name(&e.name)));
                }
                for k in keys {
                    keyed.entry(k).or_default().push((c.server, i));
                }
            }
            union_keys(&mut uf, keyed);
            // 2. Seasons and episodes of grouped series, by number.
            if self.groups_type("Series") {
                let by_upstream: HashMap<(ServerId, &str), usize> = copies
                    .iter()
                    .enumerate()
                    .map(|(i, c)| ((c.server, c.entry.id.as_str()), i))
                    .collect();
                let mut keyed: HashMap<String, Vec<(ServerId, usize)>> = HashMap::new();
                for (i, c) in copies.iter().enumerate() {
                    let e = c.entry;
                    let Some(series) = e
                        .series
                        .as_deref()
                        .and_then(|s| by_upstream.get(&(c.server, s)))
                    else {
                        continue;
                    };
                    let root = uf.find(*series);
                    let key = match (e.kind.as_str(), e.index) {
                        ("Season", Some(n)) => format!("season:{root}:{n}"),
                        ("Episode", Some(n)) => match e.parent_index {
                            Some(s) => format!("ep:{root}:{s}:{n}:{:?}", e.index_end),
                            None => continue,
                        },
                        _ => continue,
                    };
                    keyed.entry(key).or_default().push((c.server, i));
                }
                union_keys(&mut uf, keyed);
            }
        }
        let mut groups: HashMap<usize, Vec<usize>> = HashMap::new();
        for i in 0..copies.len() {
            let r = uf.find(i);
            groups.entry(r).or_default().push(i);
        }
        let mut out: Vec<Vec<usize>> = groups.into_values().collect();
        out.sort_by_key(|g| g[0]);
        out
    }
}

/// Canonical id for each group (members ranked best first), honouring the
/// persisted `member -> canonical` choices so ids handed to clients stay
/// valid. Returns the canonical per group and the rows to persist.
///
/// A group keeps a persisted canonical when it still contains that id;
/// otherwise it takes the smallest persisted canonical of its members that
/// no other group holds or owns as a member; otherwise its best member.
pub fn assign_canonical(
    groups: &[Vec<ProxyItemId>],
    persisted: &HashMap<ProxyItemId, ProxyItemId>,
) -> (Vec<ProxyItemId>, Vec<(ProxyItemId, ProxyItemId)>) {
    let owner: HashMap<&ProxyItemId, usize> = groups
        .iter()
        .enumerate()
        .flat_map(|(gi, g)| g.iter().map(move |m| (m, gi)))
        .collect();
    let mut chosen: Vec<Option<ProxyItemId>> = vec![None; groups.len()];
    let mut claimed: BTreeSet<ProxyItemId> = BTreeSet::new();
    let prefs = |g: &Vec<ProxyItemId>| -> BTreeSet<ProxyItemId> {
        g.iter().filter_map(|m| persisted.get(m).cloned()).collect()
    };
    for (gi, g) in groups.iter().enumerate() {
        if let Some(c) = prefs(g).into_iter().find(|c| g.contains(c)) {
            claimed.insert(c.clone());
            chosen[gi] = Some(c);
        }
    }
    for (gi, g) in groups.iter().enumerate() {
        if chosen[gi].is_some() {
            continue;
        }
        let c = prefs(g)
            .into_iter()
            .find(|c| !claimed.contains(c) && owner.get(c).is_none_or(|o| *o == gi))
            .unwrap_or_else(|| g[0].clone());
        claimed.insert(c.clone());
        chosen[gi] = Some(c);
    }
    let canon: Vec<ProxyItemId> = chosen.into_iter().map(|c| c.expect("set")).collect();
    let mut rows = vec![];
    for (g, c) in groups.iter().zip(&canon) {
        for m in g {
            if persisted.get(m) != Some(c)
                && !(g.len() == 1 && m == c && !persisted.contains_key(m))
            {
                rows.push((m.clone(), c.clone()));
            }
        }
    }
    (canon, rows)
}

#[cfg(test)]
#[path = "grouping_tests.rs"]
mod tests;
