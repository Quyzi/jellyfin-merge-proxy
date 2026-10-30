//! Two fake Jellyfin servers: a movie on both (deduped), one movie each,
//! and a show split across them (s1: episodes 1-2, s2: episode 3).

use jmp::backend::mock::MockBackend;
use jmp::config::UpstreamConfig;
use serde_json::{json, Value};

fn id(parts: &[&str]) -> String {
    jmp::ids::hash_id(parts)
}

fn q<'a>(q: &'a [(String, String)], k: &str) -> Option<&'a str> {
    q.iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(k))
        .map(|(_, v)| v.as_str())
}

/// One fake server's catalog: (library, item DTO).
fn catalog(server: &str) -> Vec<(&'static str, Value)> {
    let n = server;
    let name = |kind: &str, v: &str| json!({"Name": v, "Id": id(&[kind, n, v])});
    let movie = |title: &str, tmdb: &str, year: i64, genres: &[&str]| {
        json!({"Id": id(&["movie", n, title]), "Name": title, "Type": "Movie",
               "SortName": title.to_lowercase(), "ProductionYear": year,
               "ProviderIds": {"Tmdb": tmdb}, "RunTimeTicks": 6_000_000_000i64, "MediaType": "Video",
               "ImageTags": {"Primary": "tag"},
               "GenreItems": genres.iter().map(|g| name("genre", g)).collect::<Vec<_>>(),
               "Genres": genres,
               "Studios": [name("studio", "Blender")],
               "People": [{"Name": "Actor A", "Id": id(&["person", n, "Actor A"]), "Type": "Actor"}]})
    };
    let series = id(&["series", n]);
    let season = id(&["season", n]);
    let ep = |e: i64| {
        json!({"Id": id(&["ep", n, &e.to_string()]), "Name": format!("Episode {e}"), "Type": "Episode",
               "SeriesId": series, "SeriesName": "Show", "SeasonId": season, "ParentId": season,
               "ParentIndexNumber": 1, "IndexNumber": e, "RunTimeTicks": 6_000_000_000i64,
               "MediaType": "Video", "PremiereDate": format!("2020-01-0{e}T00:00:00Z")})
    };
    let mut v = vec![(
        "movies",
        movie("Big Buck Bunny", "10378", 2008, &["Animation", "Comedy"]),
    )];
    let show = vec![
        json!({"Id": series, "Name": "Show", "Type": "Series", "SortName": "show",
               "ProviderIds": {"Tvdb": "1"}, "Studios": [name("studio", "AMC")]}),
        json!({"Id": season, "Name": "Season 1", "Type": "Season", "IndexNumber": 1,
               "SeriesId": series, "ParentId": series}),
    ];
    if n == "s1" {
        v.push((
            "movies",
            movie("Sintel", "45745", 2010, &["Animation", "Fantasy"]),
        ));
        v.extend(show.into_iter().chain([ep(1), ep(2)]).map(|i| ("shows", i)));
    } else {
        v.push((
            "movies",
            movie("Tears of Steel", "133701", 2012, &["Science Fiction"]),
        ));
        v.extend(show.into_iter().chain([ep(3)]).map(|i| ("shows", i)));
    }
    v
}

/// Genre/studio/person DTOs, as `Items?Ids=` returns them.
fn name_items(server: &str) -> Vec<Value> {
    let mut out = vec![];
    for (kind, ty, names) in [
        (
            "genre",
            "Genre",
            &["Animation", "Comedy", "Fantasy", "Science Fiction"][..],
        ),
        ("studio", "Studio", &["Blender", "AMC"][..]),
        ("person", "Person", &["Actor A"][..]),
    ] {
        for v in names {
            out.push(
                json!({"Id": id(&[kind, server, v]), "Name": v, "Type": ty, "ChannelId": null,
                            "ImageTags": {"Primary": "nametag"}, "BackdropImageTags": [],
                            "ImageBlurHashes": {"Primary": {"nametag": "LEHV6n"}},
                            "UserData": {"PlaybackPositionTicks": 0, "PlayCount": 0,
                                         "IsFavorite": false, "Played": false}}),
            );
        }
    }
    out
}

pub fn install(m: &MockBackend, cfg: &UpstreamConfig) {
    let n = cfg.name.clone();
    let libs = ["movies", "shows"].map(|l| (l, id(&["lib", &n, l])));
    let views: Vec<Value> = libs
        .iter()
        .map(|(l, i)| {
            json!({"Id": i, "Name": l, "Type": "CollectionFolder",
                             "CollectionType": if *l == "movies" { "movies" } else { "tvshows" }})
        })
        .collect();
    m.on_json(
        None,
        "UserViews",
        json!({"Items": views, "TotalRecordCount": 2}),
    );
    let all = catalog(&n);
    let movies: Vec<Value> = all
        .iter()
        .filter(|(l, _)| *l == "movies")
        .map(|(_, v)| v.clone())
        .collect();
    m.on_json(None, "Items/Latest", Value::Array(movies));
    let names = name_items(&n);
    m.on_query(None, "Items", move |qs| {
        let mut items: Vec<Value> = match q(qs, "Ids") {
            Some(ids) => {
                let want: Vec<&str> = ids.split(',').collect();
                all.iter()
                    .map(|(_, v)| v)
                    .chain(names.iter())
                    .filter(|v| want.contains(&v["Id"].as_str().unwrap_or("")))
                    .cloned()
                    .collect()
            }
            None => all
                .iter()
                .filter(|(l, _)| {
                    q(qs, "ParentId").is_none_or(|p| libs.iter().any(|(ln, li)| ln == l && li == p))
                })
                .map(|(_, v)| v.clone())
                .filter(|v| {
                    q(qs, "IncludeItemTypes").is_none_or(|t| t.split(',').any(|t| v["Type"] == t))
                })
                .collect(),
        };
        for v in &mut items {
            if v.get("UserData").is_none() {
                v["UserData"] = json!({"PlaybackPositionTicks": 0, "PlayCount": 0,
                                       "IsFavorite": false, "Played": false});
            }
        }
        let total = items.len();
        let start: usize = q(qs, "StartIndex")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let limit: usize = q(qs, "Limit")
            .and_then(|s| s.parse().ok())
            .unwrap_or(usize::MAX);
        items = items.into_iter().skip(start).take(limit).collect();
        json!({"Items": items, "TotalRecordCount": total, "StartIndex": start})
    });
    for p in [
        "Sessions/Playing",
        "Sessions/Playing/Progress",
        "Sessions/Playing/Stopped",
    ] {
        m.on(
            None,
            p,
            reqwest::StatusCode::NO_CONTENT,
            "text/plain",
            Default::default(),
        );
    }
    m.on_json(
        None,
        "Items/Filters2",
        json!({"Genres": [], "Tags": [],
               "AudioLanguages": [{"Name": format!("Lang {n}"), "Value": n}],
               "SubtitleLanguages": []}),
    );
}
