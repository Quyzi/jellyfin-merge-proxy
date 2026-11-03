//! Genres, studios, people and filter values answered from the catalog
//! index: names are merged across servers (one "Drama", one id that works
//! as a filter on every server) and the lists are exact.

use super::fetch::fetch_dtos;
use super::indexed::{index_outcome, is_live, run_query, warm_snapshot};
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
    user: &str,
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
    run_query(st, snap, &iq, endpoint, user).map(|(_, all)| all)
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

/// The upstream DTOs of a page of names (images, `UserData`, ...) from a
/// live member of each, with the canonical id; the minimal DTO when no
/// member answers.
async fn hydrate_names(
    st: &Shared,
    c: &Client,
    snap: &Snapshot,
    page: &[usize],
    q: &[(String, String)],
) -> Vec<Value> {
    let picks: Vec<Option<(ServerId, String)>> = page
        .iter()
        .map(|ni| {
            let n = &snap.names[*ni];
            std::iter::once(&n.id)
                .chain(n.members.iter())
                .filter_map(|m| match st.ids.decode(m) {
                    Some(Target::Item { upstream, id }) if is_live(st, upstream) => {
                        Some((upstream, id.into_string()))
                    }
                    _ => None,
                })
                .next()
        })
        .collect();
    let mut dtos = fetch_dtos(st, c, &picks, q).await;
    picks
        .into_iter()
        .zip(page)
        .map(|(pick, ni)| match pick.and_then(|p| dtos.remove(&p)) {
            Some(mut v) => {
                v["Id"] = Value::String(snap.names[*ni].id.as_str().to_string());
                v
            }
            None => name_dto(st, snap, *ni),
        })
        .collect()
}

/// Jellyfin's per-type item counts on genre/studio DTOs (sent when the
/// client passes `IncludeItemTypes`), over every item the user can see.
fn add_counts(snap: &Snapshot, kind: NameKind, page: &[usize], all: &[usize], out: &mut [Value]) {
    const KEYS: &[(&str, &str)] = &[
        ("Movie", "MovieCount"),
        ("Series", "SeriesCount"),
        ("Episode", "EpisodeCount"),
        ("Trailer", "TrailerCount"),
        ("Program", "ProgramCount"),
        ("Audio", "SongCount"),
        ("MusicAlbum", "AlbumCount"),
        ("MusicArtist", "ArtistCount"),
        ("MusicVideo", "MusicVideoCount"),
    ];
    for (ni, v) in page.iter().zip(out.iter_mut()) {
        let mut counts = [0u64; 9];
        let mut child = 0u64;
        for &i in all {
            let it = &snap.items[i];
            // Episodes count under their series' studios, as in Jellyfin.
            let has = match kind {
                NameKind::Genre => it.genres.contains(ni),
                NameKind::Studio => {
                    it.studios.contains(ni)
                        || (snap.primary(it).is("Episode")
                            && it
                                .series
                                .is_some_and(|s| snap.items[s].studios.contains(ni)))
                }
                NameKind::Person => it.people.iter().any(|(n, _)| n == ni),
            };
            let e = snap.primary(it);
            // Seasons are not counted (Jellyfin counts the series and its
            // episodes).
            if !has || e.is("Season") {
                continue;
            }
            child += 1;
            if let Some(k) = KEYS.iter().position(|(t, _)| e.is(t)) {
                counts[k] += 1;
            }
        }
        v["ChildCount"] = json!(child);
        for ((_, key), n) in KEYS.iter().zip(counts) {
            v[*key] = json!(n);
        }
    }
}

/// `Genres`, `Studios`, `Persons` from the index.
pub async fn indexed_names(
    st: &Shared,
    r: &Req,
    c: &Client,
    kind: NameKind,
) -> Option<Result<Response>> {
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
    let items = item_scope(st, &snap, q, endpoint, NAME_PARAMS, &c.user_id)?;
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
    let search = q_get(q, "SearchTerm").map(crate::index::clean::clean);
    let mut list: Vec<(String, usize)> = hit
        .into_iter()
        .filter(|ni| snap.names[*ni].kind == kind)
        .map(|ni| (snap.names[ni].name.to_lowercase(), ni))
        .filter(|(n, ni)| {
            starts.as_ref().is_none_or(|p| n.starts_with(p.as_str()))
                && from.as_ref().is_none_or(|p| n.as_str() >= p.as_str())
                && below.as_ref().is_none_or(|p| n.as_str() < p.as_str())
                && search.as_ref().is_none_or(|s| {
                    crate::index::clean::clean(&snap.names[*ni].name).contains(s.as_str())
                })
        })
        .collect();
    if let Some(s) = &search {
        let prefix = format!("{s} ");
        list.sort_by_cached_key(|(n, ni)| {
            let c = crate::index::clean::clean(&snap.names[*ni].name);
            let rel = if c == *s {
                0
            } else if c.starts_with(&prefix) {
                1
            } else if c.starts_with(s.as_str()) {
                2
            } else {
                3
            };
            (rel, n.clone())
        });
    } else {
        list.sort();
    }
    if q_get(q, "SortOrder").is_some_and(|o| o.to_ascii_lowercase().starts_with("desc")) {
        list.reverse();
    }
    let total = list.len();
    let start: usize = q_get(q, "StartIndex")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let limit: Option<usize> = q_get(q, "Limit").and_then(|s| s.parse().ok());
    let page: Vec<usize> = list
        .into_iter()
        .skip(start)
        .take(limit.unwrap_or(usize::MAX))
        .map(|(_, ni)| ni)
        .collect();
    let mut items = hydrate_names(st, c, &snap, &page, q).await;
    let with_counts = kind != NameKind::Person && q_get(q, "IncludeItemTypes").is_some();
    if with_counts {
        // Jellyfin counts every type in the client's scope (ParentId, ...),
        // not only `IncludeItemTypes`.
        let mut drop = NAME_PARAMS.to_vec();
        drop.extend(["includeitemtypes", "excludeitemtypes", "mediatypes"]);
        let all = item_scope(st, &snap, q, endpoint, &drop, &c.user_id).unwrap_or_default();
        add_counts(&snap, kind, &page, &all, &mut items);
    }
    let user_data = q_get(q, "EnableUserData").map(|v| v.eq_ignore_ascii_case("true"));
    if !user_data.unwrap_or(kind != NameKind::Genre) {
        for v in &mut items {
            if let Some(m) = v.as_object_mut() {
                m.remove("UserData");
            }
        }
    }
    // Jellyfin reports 0, not the count, when the client opts out.
    let total =
        if q_get(q, "EnableTotalRecordCount").is_some_and(|v| v.eq_ignore_ascii_case("false")) {
            0
        } else {
            total
        };
    Some(Ok(ok_json(dto::query_result(items, total, start))))
}

/// `Items/Filters` (legacy) and `Items/Filters2` from the index.
pub async fn indexed_filters(st: &Shared, r: &Req, c: &Client, v2: bool) -> Option<Value> {
    let endpoint = "filters";
    let snap = warm_snapshot(st, endpoint)?;
    let items = item_scope(
        st,
        &snap,
        &r.query,
        endpoint,
        // The legacy endpoint ignores the program flags; Filters2 applies them.
        if v2 {
            &[]
        } else {
            &[
                "isairing", "isseries", "ismovie", "iskids", "issports", "isnews",
            ]
        },
        &c.user_id,
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
    Some(v)
}
