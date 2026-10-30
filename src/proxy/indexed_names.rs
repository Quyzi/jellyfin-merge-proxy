//! Genres, studios, people and filter values answered from the catalog
//! index: names are merged across servers (one "Drama", one id that works
//! as a filter on every server) and the lists are exact.

use super::indexed::{index_outcome, run_query, warm_snapshot};
use super::*;
use crate::index::snapshot::{NameKind, Snapshot};
use std::collections::BTreeSet;

/// Parameters that apply to the names themselves, not to the items they
/// are collected from.
const NAME_PARAMS: &[&str] = &[
    "namestartswith",
    "namestartswithorgreater",
    "namelessthan",
    "searchterm",
    "startindex",
    "limit",
    "sortby",
    "sortorder",
    "persontypes",
    "fields",
    "enableimages",
    "imagetypelimit",
    "enableimagetypes",
    "enableuserdata",
    "enabletotalrecordcount",
];

pub fn name_kind(path_head: &str) -> Option<NameKind> {
    match path_head.to_ascii_lowercase().as_str() {
        "genres" => Some(NameKind::Genre),
        "studios" => Some(NameKind::Studio),
        "persons" => Some(NameKind::Person),
        _ => None,
    }
}

/// Items matched by the client's item-level filters (always recursive).
fn item_scope(
    st: &Shared,
    snap: &Snapshot,
    q: &[(String, String)],
    endpoint: &'static str,
    drop: &[&str],
) -> Option<Vec<usize>> {
    let mut iq: Vec<(String, String)> = q
        .iter()
        .filter(|(k, _)| {
            let kl = k.to_ascii_lowercase();
            !drop.contains(&kl.as_str()) && kl != "recursive"
        })
        .cloned()
        .collect();
    iq.push(("Recursive".into(), "true".into()));
    run_query(st, snap, &iq, endpoint).map(|(_, all)| all)
}

fn name_dto(st: &Shared, snap: &Snapshot, ni: usize) -> Value {
    let n = &snap.names[ni];
    json!({
        "Name": n.name,
        "ServerId": st.server_id,
        "Id": n.id.as_str(),
        "Type": n.kind.type_name(),
        "ImageTags": {},
        "BackdropImageTags": [],
        "ImageBlurHashes": {},
        "LocationType": "FileSystem",
        "MediaType": "Unknown",
    })
}

/// `Genres`, `Studios`, `Persons` from the index.
pub async fn indexed_names(st: &Shared, r: &Req, kind: NameKind) -> Option<Result<Response>> {
    let endpoint = "named";
    let snap = warm_snapshot(st, endpoint)?;
    let q = &r.query;
    let sort_ok = q_get(q, "SortBy").is_none_or(|s| {
        s.split(',')
            .all(|k| k.eq_ignore_ascii_case("SortName") || k.eq_ignore_ascii_case("Name"))
    });
    if !sort_ok || q_get(q, "IsFavorite").is_some() {
        index_outcome(endpoint, "unsupported");
        return None;
    }
    let items = item_scope(st, &snap, q, endpoint, NAME_PARAMS)?;
    index_outcome(endpoint, "index");
    let roles: Vec<String> = q_get(q, "PersonTypes")
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    let mut hit: BTreeSet<usize> = BTreeSet::new();
    for i in items {
        let it = &snap.items[i];
        match kind {
            NameKind::Genre => hit.extend(it.genres.iter().copied()),
            NameKind::Studio => hit.extend(it.studios.iter().copied()),
            NameKind::Person => hit.extend(
                it.people
                    .iter()
                    .filter(|(_, role)| {
                        roles.is_empty() || roles.contains(&role.to_ascii_lowercase())
                    })
                    .map(|(n, _)| *n),
            ),
        }
    }
    let lower = |s: Option<&str>| s.map(|v| v.to_lowercase());
    let starts = lower(q_get(q, "NameStartsWith"));
    let from = lower(q_get(q, "NameStartsWithOrGreater"));
    let below = lower(q_get(q, "NameLessThan"));
    let search = q_get(q, "SearchTerm").map(crate::index::entry::normalize_name);
    let mut list: Vec<(String, usize)> = hit
        .into_iter()
        .filter(|ni| snap.names[*ni].kind == kind)
        .map(|ni| (snap.names[ni].name.to_lowercase(), ni))
        .filter(|(n, ni)| {
            starts.as_ref().is_none_or(|p| n.starts_with(p.as_str()))
                && from.as_ref().is_none_or(|p| n.as_str() >= p.as_str())
                && below.as_ref().is_none_or(|p| n.as_str() < p.as_str())
                && search.as_ref().is_none_or(|s| {
                    crate::index::entry::normalize_name(&snap.names[*ni].name).contains(s.as_str())
                })
        })
        .collect();
    list.sort();
    if q_get(q, "SortOrder").is_some_and(|o| o.to_ascii_lowercase().starts_with("desc")) {
        list.reverse();
    }
    let total = list.len();
    let start: usize = q_get(q, "StartIndex")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let limit: Option<usize> = q_get(q, "Limit").and_then(|s| s.parse().ok());
    let items: Vec<Value> = list
        .into_iter()
        .skip(start)
        .take(limit.unwrap_or(usize::MAX))
        .map(|(_, ni)| name_dto(st, &snap, ni))
        .collect();
    Some(Ok(ok_json(dto::query_result(items, total, start))))
}

/// `Items/Filters` (legacy) and `Items/Filters2` from the index.
pub async fn indexed_filters(st: &Shared, r: &Req, v2: bool) -> Option<Result<Response>> {
    let endpoint = "filters";
    let snap = warm_snapshot(st, endpoint)?;
    let items = item_scope(
        st,
        &snap,
        &r.query,
        endpoint,
        &[
            "isairing", "isseries", "ismovie", "iskids", "issports", "isnews",
        ],
    )?;
    index_outcome(endpoint, "index");
    let mut genres: BTreeSet<(String, usize)> = BTreeSet::new();
    let mut tags: BTreeSet<String> = BTreeSet::new();
    let mut ratings: BTreeSet<String> = BTreeSet::new();
    let mut years: BTreeSet<i64> = BTreeSet::new();
    for i in items {
        let it = &snap.items[i];
        let e = snap.primary(it);
        for g in &it.genres {
            genres.insert((snap.names[*g].name.to_lowercase(), *g));
        }
        tags.extend(e.tags.iter().cloned());
        ratings.extend(e.official.iter().cloned());
        years.extend(e.year);
    }
    let v = if v2 {
        let g: Vec<Value> = genres
            .into_iter()
            .map(|(_, g)| json!({"Name": snap.names[g].name, "Id": snap.names[g].id.as_str()}))
            .collect();
        json!({"Genres": g, "Tags": tags.into_iter().collect::<Vec<_>>()})
    } else {
        let g: Vec<&str> = genres
            .into_iter()
            .map(|(_, g)| snap.names[g].name.as_str())
            .collect();
        json!({
            "Genres": g,
            "Tags": tags.into_iter().collect::<Vec<_>>(),
            "OfficialRatings": ratings.into_iter().collect::<Vec<_>>(),
            "Years": years.into_iter().collect::<Vec<_>>(),
        })
    };
    Some(Ok(ok_json(v)))
}
