use super::*;
use crate::index::entry::{Entry, NameRef};
use crate::index::grouping::ProviderGrouping;
use crate::index::rank::{PriorityRanker, ServerRank};
use crate::index::snapshot::ServerEntries;
use crate::types::UpstreamItemId;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

fn hex(n: u32) -> String {
    format!("{n:032x}")
}

fn movie(n: u32, name: &str, tmdb: &str, genre: (&str, u32)) -> Entry {
    Entry {
        id: hex(n),
        kind: "Movie".into(),
        name: name.into(),
        sort_name: name.to_lowercase(),
        libraries: vec![hex(900)],
        providers: BTreeMap::from([("tmdb".to_string(), tmdb.to_string())]),
        genres: vec![NameRef {
            name: genre.0.into(),
            id: hex(genre.1),
            role: String::new(),
        }],
        media_type: Some("Video".into()),
        ..Default::default()
    }
}

fn ep(n: u32, series: u32, season: u32, s: i64, e: i64) -> Entry {
    Entry {
        id: hex(n),
        kind: "Episode".into(),
        name: format!("S{s}E{e}"),
        sort_name: format!("s{s}e{e}"),
        libraries: vec![hex(901)],
        series: Some(hex(series)),
        season: Some(hex(season)),
        parent_index: Some(s),
        index: Some(e),
        ..Default::default()
    }
}

fn folder(n: u32, kind: &str, series: Option<u32>, idx: Option<i64>) -> Entry {
    Entry {
        id: hex(n),
        kind: kind.into(),
        name: format!("{kind}{n}"),
        sort_name: format!("{kind}{n}").to_lowercase(),
        libraries: vec![hex(901)],
        series: series.map(hex),
        index: idx,
        is_folder: true,
        providers: if kind == "Series" {
            BTreeMap::from([("tvdb".to_string(), "1".to_string())])
        } else {
            BTreeMap::new()
        },
        ..Default::default()
    }
}

/// Server 0: A (Action, 5.0, PG), B (Drama), split show season 1.
/// Server 1: A again (Action), C (Action, no rating), show season 2.
fn snapshot() -> Snapshot {
    let mut a0 = movie(1, "Alpha", "11", ("Action", 501));
    a0.community = Some(5.0);
    a0.official = Some("PG".into());
    a0.created = Some("2024-01-02".into());
    let mut b0 = movie(2, "Bravo", "22", ("Drama", 502));
    b0.community = Some(7.5);
    b0.official = Some("R".into());
    b0.created = Some("2024-01-01".into());
    let a1 = movie(1, "Alpha", "11", ("action", 601));
    let mut c1 = movie(3, "Charlie", "33", ("Action", 601));
    c1.created = Some("2024-01-03".into());
    let s0 = vec![
        a0,
        b0,
        folder(10, "Series", None, None),
        folder(11, "Season", Some(10), Some(1)),
        ep(12, 10, 11, 1, 1),
        ep(13, 10, 11, 1, 2),
    ];
    let s1 = vec![
        a1,
        c1,
        folder(10, "Series", None, None),
        folder(21, "Season", Some(10), Some(2)),
        ep(22, 10, 21, 2, 1),
    ];
    let ratings = HashMap::from([("PG".to_string(), 10), ("R".to_string(), 17)]);
    let servers: Vec<Arc<ServerEntries>> = [s0, s1]
        .into_iter()
        .enumerate()
        .map(|(i, entries)| {
            Arc::new(ServerEntries {
                server: ServerId(i),
                entries,
                ratings: ratings.clone(),
            })
        })
        .collect();
    let encode = |s: ServerId, u: &UpstreamItemId| {
        let mut h = u.as_str().to_string();
        h.replace_range(0..1, &s.0.to_string());
        ProxyItemId::parse(&h).unwrap()
    };
    let policy = ProviderGrouping {
        enabled: true,
        types: vec!["Movie".into(), "Series".into()],
    };
    let rank = |s: ServerId| ServerRank {
        priority: s.0 as i64,
        order: 0,
    };
    Snapshot::build(
        &servers,
        &policy,
        &PriorityRanker,
        &rank,
        &encode,
        &HashMap::new(),
    )
    .0
}

fn q(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn names(snap: &Snapshot, query: &[(&str, &str)]) -> (Vec<String>, usize) {
    let all_libs = Scope::Libraries(snap.libraries.iter().cloned().collect());
    let iq = IndexQuery::parse(
        &q(query),
        snap,
        &|v| {
            ProxyItemId::parse(v)
                .and_then(|p| snap.item_index(&p))
                .map(Scope::Item)
                .or(Some(all_libs.clone()))
        },
        false,
    )
    .unwrap();
    let all = iq.run(snap, &|_| true, &|_| None);
    let page = iq
        .page(&all)
        .iter()
        .map(|i| snap.primary(&snap.items[*i]).name.clone())
        .collect();
    (page, all.len())
}

#[test]
fn exact_totals_and_paging_after_dedupe() {
    let s = snapshot();
    let base = [
        ("ParentId", "x"),
        ("Recursive", "true"),
        ("IncludeItemTypes", "Movie"),
    ];
    let (all, total) = names(&s, &base);
    assert_eq!(total, 3, "Alpha is on both servers: {all:?}");
    assert_eq!(all, ["Alpha", "Bravo", "Charlie"]);
    let mut paged = base.to_vec();
    paged.extend([("StartIndex", "2"), ("Limit", "5")]);
    assert_eq!(names(&s, &paged), (vec!["Charlie".to_string()], 3));
}

#[test]
fn sorts_by_keys_missing_from_dtos_nulls_first() {
    let s = snapshot();
    let mut base = vec![
        ("ParentId", "x"),
        ("Recursive", "true"),
        ("IncludeItemTypes", "Movie"),
    ];
    base.push(("SortBy", "CommunityRating"));
    assert_eq!(names(&s, &base).0, ["Charlie", "Alpha", "Bravo"]);
    base.pop();
    base.extend([
        ("SortBy", "OfficialRating,SortName"),
        ("SortOrder", "Descending"),
    ]);
    assert_eq!(names(&s, &base).0, ["Bravo", "Alpha", "Charlie"]);
    base.truncate(3);
    base.push(("SortBy", "DateCreated"));
    assert_eq!(names(&s, &base).0, ["Bravo", "Alpha", "Charlie"]);
}

#[test]
fn genre_filter_spans_servers() {
    let s = snapshot();
    let action = s.name_by_name(NameKind::Genre, "Action").unwrap();
    assert_eq!(s.names[action].members.len(), 2, "Action merged by name");
    let gid = s.names[action].id.to_string();
    for query in [
        vec![("Recursive", "true"), ("Genres", "action")],
        vec![("Recursive", "true"), ("GenreIds", gid.as_str())],
    ] {
        let mut query = query;
        query.push(("ParentId", "x"));
        assert_eq!(names(&s, &query).0, ["Alpha", "Charlie"]);
    }
}

#[test]
fn split_show_is_one_series() {
    let s = snapshot();
    let series = s
        .items
        .iter()
        .find(|i| s.primary(i).kind == "Series")
        .unwrap();
    assert_eq!(series.copies.len(), 2);
    let sid = series.id.to_string();
    let seasons = [("ParentId", sid.as_str()), ("IncludeItemTypes", "Season")];
    assert_eq!(names(&s, &seasons).1, 2, "season 1 + season 2");
    let eps = [
        ("SeriesId", sid.as_str()),
        ("Recursive", "true"),
        ("IncludeItemTypes", "Episode"),
        ("SortBy", "ParentIndexNumber,IndexNumber"),
    ];
    assert_eq!(names(&s, &eps).0, ["S1E1", "S1E2", "S2E1"]);
}

#[test]
fn refuses_what_it_cannot_answer() {
    let s = snapshot();
    let p = |_: &str| Some(Scope::Libraries(vec![]));
    for bad in [
        vec![("Recursive", "true"), ("HasSubtitles", "true")],
        vec![("Recursive", "true"), ("SortBy", "DatePlayed")],
        vec![("Recursive", "true"), ("IsFavorite", "true")],
        vec![("ParentId", "x")],
    ] {
        assert!(
            IndexQuery::parse(&q(&bad), &s, &p, false).is_err(),
            "{bad:?}"
        );
    }
}
