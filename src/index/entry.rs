//! [`Entry`]: what the index keeps about one item on one server.
//!
//! Only the fields needed to filter, sort, group and route are kept; the
//! full DTO is always fetched from the server that holds the copy, so the
//! client sees exactly what Jellyfin would send.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A genre, studio or person attached to an item, as named on one server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NameRef {
    pub name: String,
    /// Upstream id of the name item (genre/studio/person) on that server.
    pub id: String,
    /// Person type ("Actor", "Director", ...); empty for genres/studios.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub role: String,
}

/// One item on one server. Ids are upstream ids (normalised hex).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub sort_name: String,
    /// Upstream library (CollectionFolder) ids the item was found in.
    pub libraries: Vec<String>,
    pub series: Option<String>,
    pub season: Option<String>,
    pub index: Option<i64>,
    pub parent_index: Option<i64>,
    pub index_end: Option<i64>,
    pub year: Option<i64>,
    pub premiere: Option<String>,
    pub created: Option<String>,
    pub community: Option<f64>,
    pub critic: Option<f64>,
    pub official: Option<String>,
    pub runtime: Option<i64>,
    pub is_folder: bool,
    pub media_type: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub series_name: Option<String>,
    pub providers: BTreeMap<String, String>,
    pub genres: Vec<NameRef>,
    pub studios: Vec<NameRef>,
    pub people: Vec<NameRef>,
    pub tags: Vec<String>,
    /// The DTO's `LocationType` (`FileSystem`, `Virtual`, ...); `None` for
    /// rows crawled before it was recorded.
    pub location_type: Option<String>,
    pub original_title: Option<String>,
}

/// `Fields` the crawler asks for (everything [`Entry::from_dto`] reads that
/// Jellyfin only sends on request).
pub const CRAWL_FIELDS: &str = "ProviderIds,Genres,Studios,People,SortName,PremiereDate,\
DateCreated,ParentId,Tags,OfficialRating,Width,Height,OriginalTitle";

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(|x| x.as_str())
        .filter(|x| !x.is_empty())
        .map(String::from)
}

fn id(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(|x| x.as_str())
        .and_then(crate::types::normalize_hex_id)
}

fn i(v: &Value, k: &str) -> Option<i64> {
    v.get(k).and_then(|x| x.as_i64())
}

fn names(v: &Value, k: &str, with_role: bool) -> Vec<NameRef> {
    let mut out: Vec<NameRef> = vec![];
    for x in v.get(k).and_then(|a| a.as_array()).into_iter().flatten() {
        let (Some(name), Some(id)) = (s(x, "Name"), id(x, "Id")) else {
            continue;
        };
        let role = if with_role {
            s(x, "Type").unwrap_or_default()
        } else {
            String::new()
        };
        if !out.iter().any(|n| n.id == id && n.role == role) {
            out.push(NameRef { name, id, role });
        }
    }
    out
}

/// Video width/height: the DTO's own, else the first video stream's.
fn dimensions(v: &Value) -> (Option<i64>, Option<i64>) {
    if let (Some(w), Some(h)) = (i(v, "Width"), i(v, "Height")) {
        return (Some(w), Some(h));
    }
    let streams = v
        .get("MediaSources")
        .and_then(|m| m.get(0))
        .and_then(|m| m.get("MediaStreams"))
        .and_then(|m| m.as_array());
    for st in streams.into_iter().flatten() {
        if st.get("Type").and_then(|t| t.as_str()) == Some("Video") {
            return (i(st, "Width"), i(st, "Height"));
        }
    }
    (None, None)
}

impl Entry {
    /// Build from an upstream DTO (raw upstream ids). None without an id.
    pub fn from_dto(v: &Value, library: Option<&str>) -> Option<Entry> {
        let item_id = id(v, "Id")?;
        let (width, height) = dimensions(v);
        let providers = v
            .get("ProviderIds")
            .and_then(|p| p.as_object())
            .map(|o| {
                o.iter()
                    .filter_map(|(k, x)| {
                        let x = x.as_str()?.trim();
                        (!x.is_empty()).then(|| (k.to_ascii_lowercase(), x.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let name = s(v, "Name").unwrap_or_default();
        Some(Entry {
            id: item_id,
            kind: s(v, "Type").unwrap_or_default(),
            sort_name: s(v, "SortName").unwrap_or_else(|| name.to_lowercase()),
            name,
            libraries: library.map(|l| vec![l.to_string()]).unwrap_or_default(),
            series: id(v, "SeriesId"),
            season: id(v, "SeasonId").or_else(|| {
                // Episodes outside a season folder report only ParentId.
                (s(v, "Type").as_deref() == Some("Episode"))
                    .then(|| id(v, "ParentId"))
                    .flatten()
                    .filter(|p| Some(p) != id(v, "SeriesId").as_ref())
            }),
            index: i(v, "IndexNumber"),
            parent_index: i(v, "ParentIndexNumber"),
            index_end: i(v, "IndexNumberEnd"),
            year: i(v, "ProductionYear"),
            premiere: s(v, "PremiereDate"),
            created: s(v, "DateCreated"),
            community: v.get("CommunityRating").and_then(|x| x.as_f64()),
            critic: v.get("CriticRating").and_then(|x| x.as_f64()),
            official: s(v, "OfficialRating"),
            runtime: i(v, "RunTimeTicks"),
            is_folder: v.get("IsFolder").and_then(|x| x.as_bool()).unwrap_or(false),
            media_type: s(v, "MediaType"),
            width,
            height,
            series_name: s(v, "SeriesName"),
            providers,
            genres: names(v, "GenreItems", false),
            studios: names(v, "Studios", false),
            people: names(v, "People", true),
            tags: v
                .get("Tags")
                .and_then(|t| t.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            location_type: s(v, "LocationType"),
            original_title: s(v, "OriginalTitle"),
        })
    }

    /// Fold another sighting of the same item (found in another library).
    pub fn absorb(&mut self, other: Entry) {
        for l in other.libraries {
            if !self.libraries.contains(&l) {
                self.libraries.push(l);
            }
        }
    }

    pub fn is(&self, kind: &str) -> bool {
        self.kind.eq_ignore_ascii_case(kind)
    }

    /// Pixel count of the main video stream (0 if unknown).
    pub fn pixels(&self) -> i64 {
        self.width.unwrap_or(0) * self.height.unwrap_or(0)
    }
}

/// Normalised name for matching genres/studios/people across servers.
pub fn normalize_name(n: &str) -> String {
    n.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_episode_dto() {
        let v = json!({"Id":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","Type":"Episode","Name":"Pilot",
            "SeriesId":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","SeasonId":"cccccccccccccccccccccccccccccccc",
            "IndexNumber":1,"ParentIndexNumber":1,"ProviderIds":{"Tvdb":"349232","Imdb":""},
            "GenreItems":[{"Name":"Drama","Id":"dddddddddddddddddddddddddddddddd"}],
            "People":[{"Name":"A","Id":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee","Type":"Actor"}],
            "MediaSources":[{"MediaStreams":[{"Type":"Video","Width":640,"Height":360}]}]});
        let e = Entry::from_dto(&v, Some("ffffffffffffffffffffffffffffffff")).unwrap();
        assert_eq!(e.id, "a".repeat(32));
        assert_eq!(e.sort_name, "pilot");
        assert_eq!(e.season.as_deref(), Some("c".repeat(32).as_str()));
        assert_eq!(e.providers.len(), 1);
        assert_eq!(e.pixels(), 640 * 360);
        assert_eq!(e.genres[0].name, "Drama");
        assert_eq!(e.people[0].role, "Actor");
        let back: Entry = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn names_normalise() {
        assert_eq!(normalize_name("  Science   Fiction "), "science fiction");
    }
}
