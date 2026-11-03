//! Item queries answered from the index: the subset of `/Items` query
//! parameters the index understands, with Jellyfin's filter semantics.
//!
//! [`IndexQuery::parse`] refuses (`Err(reason)`) any parameter it does not
//! know, so the caller falls back to live fan-out instead of answering a
//! query differently from Jellyfin.

use super::clean::clean;
use super::snapshot::{Item, NameKind, Snapshot};
use super::sort::{parse_sort, SortCtx, SortKey};
use crate::types::{normalize_hex_id, ProxyItemId, ServerId};

#[path = "query_match.rs"]
mod query_match;

/// Per-user state of one item (from the proxy's user data store).
pub use crate::userdata::ItemState as UserFact;

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
    /// `SearchTerm` after [`clean`], and its plain lower-case form (matched
    /// against `OriginalTitle`).
    pub search: Option<String>,
    pub search_original: Option<String>,
    pub virtual_item: Option<bool>,
    pub missing: Option<bool>,
    pub unaired: Option<bool>,
    pub exclude_tags: Vec<String>,
    /// Program flags that can never hold for library items
    /// (`IsAiring`, `IsSeries=true`, `IsMovie=false`, ...).
    pub impossible: bool,
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
    "isvirtualunaired",
    "collapseboxsetitems",
    "hasquerylimit",
    "format",
    "excludeactivesessions",
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
        let (mut exclude_loc, mut loc) = (vec![], vec![]);
        let (mut movie, mut series, mut airing) = (None, None, None);
        let (mut sports, mut news, mut kids) = (None, None, None);
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
                "searchterm" => {
                    out.search = Some(clean(v));
                    out.search_original = Some(v.to_lowercase());
                }
                "ismissing" => out.missing = truthy(v),
                "isunaired" => out.unaired = truthy(v),
                "excludelocationtypes" => exclude_loc = list(v, ','),
                "locationtypes" => loc = list(v, ','),
                "ismovie" => movie = truthy(v),
                "isseries" => series = truthy(v),
                "isairing" => airing = truthy(v),
                "issports" => sports = truthy(v),
                "isnews" => news = truthy(v),
                "iskids" => kids = truthy(v),
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
        let movie_ok = out.include_types.is_empty()
            || out
                .include_types
                .iter()
                .any(|t| t.eq_ignore_ascii_case("Movie") || t.eq_ignore_ascii_case("Trailer"));
        if movie.is_some_and(|m| !(m && movie_ok)) || series == Some(true) || airing.is_some() {
            out.impossible = true;
        }
        for (flag, tag) in [(sports, "sports"), (news, "news"), (kids, "kids")] {
            match flag {
                Some(true) => out.tags.push(tag.into()),
                Some(false) => out.exclude_tags.push(tag.into()),
                None => {}
            }
        }
        for t in out.tags.iter_mut().chain(out.exclude_tags.iter_mut()) {
            *t = clean(t);
        }
        if exclude_loc
            .iter()
            .any(|t| t.eq_ignore_ascii_case("Virtual"))
        {
            out.virtual_item = Some(false);
        }
        if (1..4).contains(&loc.len()) {
            out.virtual_item = Some(loc.iter().any(|t| t.eq_ignore_ascii_case("Virtual")));
        }
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
        ctx.sort(&mut hits, &self.sort, self.search.as_deref());
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
pub(crate) mod tests;
