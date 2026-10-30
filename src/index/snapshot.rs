//! [`Snapshot`]: the merged catalog built from every server's entries.
//!
//! Immutable once built; the [`super::IndexService`] swaps in a new one
//! after each crawl. Each [`Item`] is one canonical item (a group of
//! copies); each [`NameItem`] one genre/studio/person merged by name.

use super::entry::{normalize_name, Entry};
use super::grouping::{assign_canonical, Copy, GroupingPolicy};
use super::rank::{CopyRanker, ServerRank};
use crate::index::groups::GroupMap;
use crate::types::{ProxyItemId, ServerId, UpstreamItemId};
use std::collections::HashMap;
use std::sync::Arc;

/// Everything indexed from one server.
#[derive(Debug, Clone)]
pub struct ServerEntries {
    pub server: ServerId,
    pub entries: Vec<Entry>,
    /// Parental rating name -> score, from the server's localization.
    pub ratings: HashMap<String, i64>,
}

/// One copy of an item.
#[derive(Debug, Clone)]
pub struct CopyRec {
    pub server: ServerId,
    pub pid: ProxyItemId,
    pub entry: Entry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum NameKind {
    Genre,
    Studio,
    Person,
}

impl NameKind {
    pub fn type_name(self) -> &'static str {
        match self {
            NameKind::Genre => "Genre",
            NameKind::Studio => "Studio",
            NameKind::Person => "Person",
        }
    }
}

/// A genre/studio/person, merged across servers by normalised name.
#[derive(Debug, Clone)]
pub struct NameItem {
    pub id: ProxyItemId,
    pub kind: NameKind,
    pub name: String,
    pub members: Vec<ProxyItemId>,
}

/// One canonical item.
#[derive(Debug, Clone)]
pub struct Item {
    pub id: ProxyItemId,
    /// Indexes into [`Snapshot::copies`], best copy first.
    pub copies: Vec<usize>,
    pub series: Option<usize>,
    pub season: Option<usize>,
    /// (server, upstream library id) of every copy.
    pub libraries: Vec<(ServerId, String)>,
    /// Name items (indexes into [`Snapshot::names`]); people with roles.
    pub genres: Vec<usize>,
    pub studios: Vec<usize>,
    pub people: Vec<(usize, String)>,
    pub rating_score: Option<i64>,
}

#[derive(Debug, Default)]
pub struct Snapshot {
    pub copies: Vec<CopyRec>,
    pub items: Vec<Item>,
    pub names: Vec<NameItem>,
    by_id: HashMap<ProxyItemId, usize>,
    name_by_id: HashMap<ProxyItemId, usize>,
    name_by_key: HashMap<(NameKind, String), usize>,
    pub groups: GroupMap,
    /// Every (server, library id) that holds indexed items.
    pub libraries: std::collections::HashSet<(ServerId, String)>,
    /// Episodes of each series and season (item indexes), for per-user
    /// folder state (played / unplayed counts) and NextUp.
    episodes: HashMap<usize, Vec<usize>>,
}

/// What a build produced besides the snapshot.
#[derive(Debug, Default)]
pub struct BuildOutput {
    /// `member -> canonical` rows to persist.
    pub canonical_rows: Vec<(ProxyItemId, ProxyItemId)>,
    /// Item groups with more than one copy.
    pub merged_groups: usize,
}

impl Snapshot {
    /// Build from every server's entries. `encode` maps an upstream id to
    /// its proxy id (registering it), `persisted` holds earlier canonical
    /// choices.
    pub fn build(
        servers: &[Arc<ServerEntries>],
        policy: &dyn GroupingPolicy,
        ranker: &dyn CopyRanker,
        rank: &dyn Fn(ServerId) -> ServerRank,
        encode: &dyn Fn(ServerId, &UpstreamItemId) -> ProxyItemId,
        persisted: &HashMap<ProxyItemId, ProxyItemId>,
    ) -> (Snapshot, BuildOutput) {
        let mut copies = vec![];
        for s in servers {
            for e in &s.entries {
                let Some(uid) = UpstreamItemId::parse(&e.id) else {
                    continue;
                };
                copies.push(CopyRec {
                    server: s.server,
                    pid: encode(s.server, &uid),
                    entry: e.clone(),
                });
            }
        }
        let view: Vec<Copy> = copies
            .iter()
            .map(|c| Copy {
                server: c.server,
                entry: &c.entry,
            })
            .collect();
        let mut groups = policy.group(&view);
        for g in groups.iter_mut() {
            g.sort_by(|a, b| ranker.compare(&copies[*a], &copies[*b], rank));
        }
        let ids: Vec<Vec<ProxyItemId>> = groups
            .iter()
            .map(|g| g.iter().map(|i| copies[*i].pid.clone()).collect())
            .collect();
        let (canon, mut rows) = assign_canonical(&ids, persisted);
        let mut out = BuildOutput {
            merged_groups: groups.iter().filter(|g| g.len() > 1).count(),
            ..Default::default()
        };
        let mut map = GroupMap::new();
        for (g, c) in ids.iter().zip(&canon) {
            map.insert(c.clone(), g.clone());
        }
        let mut snap = Snapshot::default();
        let mut copy_item: Vec<usize> = vec![0; copies.len()];
        for (gi, (g, c)) in groups.iter().zip(&canon).enumerate() {
            for i in g {
                copy_item[*i] = gi;
            }
            snap.by_id.insert(c.clone(), gi);
            for m in &ids[gi] {
                snap.by_id.insert(m.clone(), gi);
            }
            snap.items.push(Item {
                id: c.clone(),
                copies: g.clone(),
                series: None,
                season: None,
                libraries: vec![],
                genres: vec![],
                studios: vec![],
                people: vec![],
                rating_score: None,
            });
        }
        // Parents, libraries, ratings.
        let by_upstream: HashMap<(ServerId, &str), usize> = copies
            .iter()
            .enumerate()
            .map(|(i, c)| ((c.server, c.entry.id.as_str()), i))
            .collect();
        let ratings: HashMap<ServerId, &HashMap<String, i64>> =
            servers.iter().map(|s| (s.server, &s.ratings)).collect();
        for item in snap.items.iter_mut() {
            let first = &copies[item.copies[0]];
            let parent = |up: &Option<String>| {
                up.as_deref()
                    .and_then(|s| by_upstream.get(&(first.server, s)))
                    .map(|ci| copy_item[*ci])
            };
            item.series = parent(&first.entry.series);
            item.season = parent(&first.entry.season);
            item.rating_score = first.entry.official.as_ref().and_then(|r| {
                ratings
                    .get(&first.server)
                    .and_then(|m| m.get(r).or_else(|| m.get(&r.to_ascii_uppercase())))
                    .copied()
            });
            for ci in &item.copies {
                let c = &copies[*ci];
                for l in &c.entry.libraries {
                    item.libraries.push((c.server, l.clone()));
                    snap.libraries.insert((c.server, l.clone()));
                }
            }
        }
        for (i, item) in snap.items.iter().enumerate() {
            if copies[item.copies[0]].entry.is("Episode") {
                for p in [item.series, item.season].into_iter().flatten() {
                    snap.episodes.entry(p).or_default().push(i);
                }
            }
        }
        rows.extend(snap.build_names(&copies, encode, persisted, &mut map));
        out.canonical_rows = rows;
        snap.copies = copies;
        snap.groups = map;
        (snap, out)
    }

    /// Merge genres/studios/people by name and attach them to items.
    fn build_names(
        &mut self,
        copies: &[CopyRec],
        encode: &dyn Fn(ServerId, &UpstreamItemId) -> ProxyItemId,
        persisted: &HashMap<ProxyItemId, ProxyItemId>,
        map: &mut GroupMap,
    ) -> Vec<(ProxyItemId, ProxyItemId)> {
        let mut keys: Vec<(NameKind, String)> = vec![];
        let mut display: Vec<String> = vec![];
        let mut members: Vec<Vec<ProxyItemId>> = vec![];
        let mut key_idx: HashMap<(NameKind, String), usize> = HashMap::new();
        let mut attach: Vec<Vec<(NameKind, usize, String)>> = vec![vec![]; self.items.len()];
        for (ii, item) in self.items.iter().enumerate() {
            for ci in &item.copies {
                let c = &copies[*ci];
                let lists = [
                    (NameKind::Genre, &c.entry.genres),
                    (NameKind::Studio, &c.entry.studios),
                    (NameKind::Person, &c.entry.people),
                ];
                for (kind, list) in lists {
                    for n in list.iter() {
                        let Some(uid) = UpstreamItemId::parse(&n.id) else {
                            continue;
                        };
                        let key = (kind, normalize_name(&n.name));
                        let ni = *key_idx.entry(key.clone()).or_insert_with(|| {
                            keys.push(key);
                            display.push(n.name.clone());
                            members.push(vec![]);
                            members.len() - 1
                        });
                        let pid = encode(c.server, &uid);
                        if !members[ni].contains(&pid) {
                            members[ni].push(pid);
                        }
                        let a = (kind, ni, n.role.clone());
                        if !attach[ii].contains(&a) {
                            attach[ii].push(a);
                        }
                    }
                }
            }
        }
        let (canon, rows) = assign_canonical(&members, persisted);
        for (ni, c) in canon.into_iter().enumerate() {
            map.insert(c.clone(), members[ni].clone());
            self.name_by_id.insert(c.clone(), ni);
            for m in &members[ni] {
                self.name_by_id.insert(m.clone(), ni);
            }
            self.name_by_key.insert(keys[ni].clone(), ni);
            self.names.push(NameItem {
                id: c,
                kind: keys[ni].0,
                name: std::mem::take(&mut display[ni]),
                members: std::mem::take(&mut members[ni]),
            });
        }
        for (ii, list) in attach.into_iter().enumerate() {
            let item = &mut self.items[ii];
            for (kind, ni, role) in list {
                match kind {
                    NameKind::Genre if !item.genres.contains(&ni) => item.genres.push(ni),
                    NameKind::Studio if !item.studios.contains(&ni) => item.studios.push(ni),
                    NameKind::Person => item.people.push((ni, role)),
                    _ => {}
                }
            }
        }
        rows
    }

    /// The canonical item for a canonical or member id.
    pub fn item(&self, id: &ProxyItemId) -> Option<&Item> {
        self.by_id.get(id).map(|i| &self.items[*i])
    }

    pub fn item_index(&self, id: &ProxyItemId) -> Option<usize> {
        self.by_id.get(id).copied()
    }

    /// The copy used for an item's metadata (its best copy).
    pub fn primary(&self, item: &Item) -> &Entry {
        &self.copies[item.copies[0]].entry
    }

    pub fn name(&self, id: &ProxyItemId) -> Option<usize> {
        self.name_by_id.get(id).copied()
    }

    pub fn name_by_name(&self, kind: NameKind, name: &str) -> Option<usize> {
        self.name_by_key.get(&(kind, normalize_name(name))).copied()
    }

    /// Episodes (item indexes) of a series or season, in index order.
    pub fn episodes_of(&self, idx: usize) -> &[usize] {
        self.episodes.get(&idx).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}
