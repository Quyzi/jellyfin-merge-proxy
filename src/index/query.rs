//! Item queries answered from the index: the subset of `/Items` query
//! parameters the index understands, with Jellyfin's filter semantics.
//!
//! [`IndexQuery::parse`] refuses (`Err(reason)`) any parameter it does not
//! know, so the caller falls back to live fan-out instead of answering a
//! query differently from Jellyfin.

use super::snapshot::{Item, NameKind, Snapshot};
use super::sort::{parse_sort, SortCtx, SortKey};
use crate::types::{normalize_hex_id, ProxyItemId, ServerId};

/// Per-user state of one item (from the proxy's user data store).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UserFact {
    pub played: bool,
    pub play_count: i64,
    pub position_ticks: i64,
    pub favorite: bool,
    pub likes: Option<bool>,
    pub last_played: Option<String>,
}

/// What a `ParentId` points at.
#[derive(Debug, Clone)]
pub enum Scope {
    /// A library (merged or not): (server, upstream library id) members.
    Libraries(Vec<(ServerId, String)>),
    /// A canonical item (series or season).
    Item(usize),
    /// An id the index knows nothing about: no results.
    Nothing,
}

#[derive(Debug, Clone, Default)]
pub struct IndexQuery {
    pub parent: Option<Scope>,
    pub recursive: bool,
    pub include_types: Vec<String>,
    pub exclude_types: Vec<String>,
    pub media_types: Vec<String>,
    pub genres: Option<Vec<usize>>,
    pub studios: Option<Vec<usize>>,
    pub persons: Option<Vec<usize>>,
    pub person_types: Vec<String>,
    pub years: Vec<i64>,
    pub official: Vec<String>,
    pub tags: Vec<String>,
    pub name_starts_with: Option<String>,
    pub name_starts_with_or_greater: Option<String>,
    pub name_less_than: Option<String>,
    pub search: Option<String>,
    pub ids: Option<Vec<usize>>,
    pub is_folder: Option<bool>,
    pub min_community: Option<f64>,
    pub min_critic: Option<f64>,
    pub series: Option<usize>,
    pub season: Option<usize>,
    pub season_number: Option<i64>,
    pub played: Option<bool>,
    pub favorite: Option<bool>,
    pub resumable: bool,
    pub likes: Option<bool>,
    pub favorite_or_likes: bool,
    pub sort: Vec<(SortKey, bool)>,
    pub start: usize,
    pub limit: Option<usize>,
}

/// Parameters that only shape the DTOs (passed on when hydrating) or that
/// the index already satisfies.
const PASSTHROUGH: &[&str] = &[
    "fields",
    "enableimages",
    "imagetypelimit",
    "enableimagetypes",
    "enableuserdata",
    "enabletotalrecordcount",
    "userid",
    "api_key",
    "apikey",
    "startindex",
    "limit",
    "sortby",
    "sortorder",
    "recursive",
    "parentid",
    "ismissing",
    "excludelocationtypes",
    "isvirtualunaired",
    "collapseboxsetitems",
    "hasquerylimit",
    "format",
    "_",
];

fn list(v: &str, sep: char) -> Vec<String> {
    v.split(sep)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn truthy(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

impl IndexQuery {
    /// Parse a client query. `parent` resolves a ParentId/SeriesId/SeasonId
    /// value; `with_user` = per-user filters/sorts can be answered.
    pub fn parse(
        q: &[(String, String)],
        snap: &Snapshot,
        parent: &dyn Fn(&str) -> Option<Scope>,
        with_user: bool,
    ) -> Result<IndexQuery, String> {
        let mut out = IndexQuery::default();
        let get = |k: &str| crate::proxy::q_get(q, k);
        let names = |raw: &str, kind: NameKind, by_id: bool| -> Vec<usize> {
            let sep = if by_id { ',' } else { '|' };
            list(raw, sep)
                .iter()
                .filter_map(|n| {
                    if by_id {
                        ProxyItemId::parse(n).and_then(|p| snap.name(&p))
                    } else {
                        snap.name_by_name(kind, n)
                    }
                })
                .collect()
        };
        for (k, v) in q {
            let kl = k.to_ascii_lowercase();
            if v.is_empty() || PASSTHROUGH.contains(&kl.as_str()) {
                continue;
            }
            match kl.as_str() {
                "includeitemtypes" => out.include_types = list(v, ','),
                "excludeitemtypes" => out.exclude_types = list(v, ','),
                "mediatypes" => out.media_types = list(v, ','),
                "genres" => out.genres = Some(names(v, NameKind::Genre, false)),
                "genreids" => out.genres = Some(names(v, NameKind::Genre, true)),
                "studios" => out.studios = Some(names(v, NameKind::Studio, false)),
                "studioids" => out.studios = Some(names(v, NameKind::Studio, true)),
                "personids" => out.persons = Some(names(v, NameKind::Person, true)),
                "person" => out.persons = Some(names(v, NameKind::Person, false)),
                "persontypes" => out.person_types = list(v, ','),
                "years" => out.years = list(v, ',').iter().filter_map(|y| y.parse().ok()).collect(),
                "officialratings" => out.official = list(v, '|'),
                "tags" => out.tags = list(v, '|'),
                "namestartswith" => out.name_starts_with = Some(v.to_lowercase()),
                "namestartswithorgreater" => {
                    out.name_starts_with_or_greater = Some(v.to_lowercase())
                }
                "namelessthan" => out.name_less_than = Some(v.to_lowercase()),
                "searchterm" => out.search = Some(super::entry::normalize_name(v)),
                "ids" => {
                    out.ids = Some(
                        list(v, ',')
                            .iter()
                            .filter_map(|i| ProxyItemId::parse(i).and_then(|p| snap.item_index(&p)))
                            .collect(),
                    )
                }
                "isfolder" => out.is_folder = truthy(v),
                "mincommunityrating" => out.min_community = v.parse().ok(),
                "mincriticrating" => out.min_critic = v.parse().ok(),
                "seriesid" | "seasonid" => match parent(v) {
                    Some(Scope::Item(i)) if kl == "seriesid" => out.series = Some(i),
                    Some(Scope::Item(i)) => out.season = Some(i),
                    _ => out.parent = Some(Scope::Nothing),
                },
                "season" | "parentindexnumber" => out.season_number = v.parse().ok(),
                "isplayed" if with_user => out.played = truthy(v),
                "isfavorite" if with_user => out.favorite = truthy(v),
                "filters" => {
                    for f in list(v, ',') {
                        match f.to_ascii_lowercase().as_str() {
                            "isfolder" => out.is_folder = Some(true),
                            "isnotfolder" => out.is_folder = Some(false),
                            "isplayed" if with_user => out.played = Some(true),
                            "isunplayed" if with_user => out.played = Some(false),
                            "isfavorite" if with_user => out.favorite = Some(true),
                            "isresumable" if with_user => out.resumable = true,
                            "likes" if with_user => out.likes = Some(true),
                            "dislikes" if with_user => out.likes = Some(false),
                            "isfavoriteorlikes" if with_user => out.favorite_or_likes = true,
                            other => return Err(format!("filter:{other}")),
                        }
                    }
                }
                _ => return Err(kl),
            }
        }
        if get("CollapseBoxSetItems").and_then(truthy) == Some(true) {
            return Err("collapseboxsetitems".into());
        }
        out.recursive = get("Recursive").and_then(truthy).unwrap_or(false);
        if let Some(p) = get("ParentId") {
            out.parent = Some(parent(p).ok_or("parentid")?);
        }
        if !out.recursive && !matches!(out.parent, Some(Scope::Item(_)) | Some(Scope::Nothing)) {
            // Direct children of a library or of nothing: only the server
            // knows its folder layout.
            if out.series.is_none() && out.season.is_none() && out.ids.is_none() {
                return Err("non_recursive".into());
            }
        }
        out.sort = parse_sort(get("SortBy"), get("SortOrder")).map_err(|k| format!("sort:{k}"))?;
        if !with_user && out.sort.iter().any(|(k, _)| k.needs_user()) {
            return Err("sort:user".into());
        }
        if out.sort.is_empty() {
            out.sort = vec![(SortKey::SortName, false)];
        }
        out.start = get("StartIndex").and_then(|s| s.parse().ok()).unwrap_or(0);
        out.limit = get("Limit").and_then(|s| s.parse().ok());
        Ok(out)
    }

    fn type_ok(&self, kind: &str, list: &[String]) -> bool {
        list.iter().any(|t| t.eq_ignore_ascii_case(kind))
    }

    fn matches(
        &self,
        snap: &Snapshot,
        idx: usize,
        user: &dyn Fn(&Item) -> Option<UserFact>,
    ) -> bool {
        let it = &snap.items[idx];
        let e = snap.primary(it);
        match &self.parent {
            None => {}
            Some(Scope::Nothing) => return false,
            Some(Scope::Libraries(libs)) => {
                if !it.libraries.iter().any(|l| libs.contains(l)) {
                    return false;
                }
            }
            Some(Scope::Item(p)) => {
                let child = it.season == Some(*p)
                    || (it.series == Some(*p) && (self.recursive || e.is("Season")));
                if !child {
                    return false;
                }
            }
        }
        if !self.include_types.is_empty() && !self.type_ok(&e.kind, &self.include_types) {
            return false;
        }
        if self.type_ok(&e.kind, &self.exclude_types) {
            return false;
        }
        if !self.media_types.is_empty()
            && !e
                .media_type
                .as_deref()
                .is_some_and(|m| self.type_ok(m, &self.media_types))
        {
            return false;
        }
        let any = |want: &Option<Vec<usize>>, have: &[usize]| {
            want.as_ref()
                .is_none_or(|w| w.iter().any(|g| have.contains(g)))
        };
        if !any(&self.genres, &it.genres) || !any(&self.studios, &it.studios) {
            return false;
        }
        if let Some(w) = &self.persons {
            let hit = it.people.iter().any(|(n, role)| {
                w.contains(n)
                    && (self.person_types.is_empty() || self.type_ok(role, &self.person_types))
            });
            if !hit {
                return false;
            }
        }
        if !self.years.is_empty() && !e.year.is_some_and(|y| self.years.contains(&y)) {
            return false;
        }
        if !self.official.is_empty()
            && !e
                .official
                .as_deref()
                .is_some_and(|r| self.type_ok(r, &self.official))
        {
            return false;
        }
        if !self.tags.is_empty() && !e.tags.iter().any(|t| self.type_ok(t, &self.tags)) {
            return false;
        }
        let sn = e.sort_name.as_str();
        if let Some(p) = &self.name_starts_with {
            if !sn.starts_with(p.as_str()) && !e.name.to_lowercase().starts_with(p.as_str()) {
                return false;
            }
        }
        if self
            .name_starts_with_or_greater
            .as_ref()
            .is_some_and(|p| sn < p.as_str())
            || self
                .name_less_than
                .as_ref()
                .is_some_and(|p| sn >= p.as_str())
        {
            return false;
        }
        if let Some(s) = &self.search {
            if !super::entry::normalize_name(&e.name).contains(s.as_str()) {
                return false;
            }
        }
        if self.ids.as_ref().is_some_and(|ids| !ids.contains(&idx))
            || self.is_folder.is_some_and(|f| f != e.is_folder)
            || self
                .min_community
                .is_some_and(|m| e.community.is_none_or(|c| c < m))
            || self
                .min_critic
                .is_some_and(|m| e.critic.is_none_or(|c| c < m))
            || self.series.is_some_and(|s| it.series != Some(s))
            || self.season.is_some_and(|s| it.season != Some(s))
            || self
                .season_number
                .is_some_and(|n| e.parent_index != Some(n))
        {
            return false;
        }
        let needs_user = self.played.is_some()
            || self.favorite.is_some()
            || self.resumable
            || self.likes.is_some()
            || self.favorite_or_likes;
        if needs_user {
            let u = user(it).unwrap_or_default();
            if self.played.is_some_and(|p| p != u.played)
                || self.favorite.is_some_and(|f| f != u.favorite)
                || (self.resumable && u.position_ticks <= 0)
                || self.likes.is_some_and(|l| u.likes != Some(l))
                || (self.favorite_or_likes && !(u.favorite || u.likes == Some(true)))
            {
                return false;
            }
        }
        true
    }

    /// Matching item indexes (sorted, all pages) among items with a copy
    /// on a live server.
    pub fn run(
        &self,
        snap: &Snapshot,
        live: &dyn Fn(ServerId) -> bool,
        user: &dyn Fn(&Item) -> Option<UserFact>,
    ) -> Vec<usize> {
        let mut hits: Vec<usize> = (0..snap.items.len())
            .filter(|i| {
                snap.items[*i]
                    .copies
                    .iter()
                    .any(|c| live(snap.copies[*c].server))
            })
            .filter(|i| self.matches(snap, *i, user))
            .collect();
        let ctx = SortCtx::new(snap, user, &self.sort, &hits);
        ctx.sort(&mut hits, &self.sort);
        hits
    }

    /// The requested page of `all`.
    pub fn page<'a>(&self, all: &'a [usize]) -> &'a [usize] {
        let s = self.start.min(all.len());
        let e = match self.limit {
            Some(l) => (s + l).min(all.len()),
            None => all.len(),
        };
        &all[s..e]
    }
}

/// Normalised hex form of a query id value.
pub fn norm(v: &str) -> Option<String> {
    normalize_hex_id(v)
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
