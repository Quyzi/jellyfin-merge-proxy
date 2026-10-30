use super::*;
use serde_json::json;

#[test]
fn sort_and_page() {
    let mut items = vec![
        json!({"Name": "Ep 10", "PremiereDate": "2020-01-01"}),
        json!({"Name": "ep 2", "PremiereDate": "2021-01-01"}),
        json!({"Name": "Alpha"}),
        json!({"Name": "Ep 1", "SortName": "zzz"}),
    ];
    sort_items(&mut items, &parse_sort(Some("SortName"), Some("Ascending")));
    let names: Vec<_> = items.iter().map(|i| i["Name"].as_str().unwrap()).collect();
    assert_eq!(names, vec!["Alpha", "ep 2", "Ep 10", "Ep 1"]);
    sort_items(
        &mut items,
        &parse_sort(Some("PremiereDate,SortName"), Some("Descending")),
    );
    assert_eq!(items[0]["Name"], "ep 2");
    assert_eq!(items[1]["Name"], "Ep 10");
    let p = page(items, 1, Some(2));
    assert_eq!(p.len(), 2);
    assert_eq!(p[0]["Name"], "Ep 10");
}

#[test]
fn dedupe_prefers_priority() {
    let r = |p: i64, s: usize, item: Value| Ranked {
        priority: p,
        server: ServerId(s),
        item,
    };
    let items = vec![
        r(
            1,
            1,
            json!({"Name": "M", "Type": "Movie", "ProviderIds": {"Tmdb": "1"}, "S": "b"}),
        ),
        r(
            0,
            0,
            json!({"Name": "M", "Type": "Movie", "ProviderIds": {"Tmdb": "1", "Imdb": "tt1"}, "S": "a"}),
        ),
        r(
            2,
            2,
            json!({"Name": "M", "Type": "Movie", "ProviderIds": {"Imdb": "tt1"}, "S": "c"}),
        ),
        r(
            2,
            2,
            json!({"Name": "Other", "Type": "Movie", "ProviderIds": {"Tmdb": "2"}}),
        ),
        r(
            2,
            2,
            json!({"Name": "Ep", "Type": "Episode", "ProviderIds": {"Tmdb": "1"}}),
        ),
    ];
    let names = vec!["a".to_string(), "b".into(), "c".into()];
    let d = MeteredDeduplicator::new(
        ProviderIdDeduplicator {
            types: vec!["Movie".to_string()],
            name_year_fallback: false,
        },
        &names,
    );
    let (out, stats) = d.dedupe(items.clone());
    assert_eq!(stats.removed, 2);
    let kinds: Vec<_> = stats.drops.iter().map(|d| d.key).collect();
    assert_eq!(kinds, vec!["tmdb", "imdb"]);
    assert!(
        stats.drops.iter().all(|d| d.winner == ServerId(0)),
        "winner is the priority-0 item"
    );
    assert_eq!(out.len(), 3);
    assert_eq!(out[0].item["S"], "a");
    let (out, stats) = NoDedupe.dedupe(items.clone());
    assert_eq!((out.len(), stats.removed), (5, 0));
    let (out, stats) = NameDeduplicator.dedupe(items);
    assert_eq!(out.len(), 3);
    assert_eq!(stats.removed, 2);
    assert_eq!(out[0].item["S"], "a");
}

/// Regression (real Jellyfin 12.3 search hints): hints have no
/// ProviderIds, so they dedupe by type + name + year when enabled, and
/// items that have provider ids never fall back to the name.
#[test]
fn search_hints_dedupe_by_name_and_year() {
    let r = |p: i64, s: usize, v: Value| Ranked {
        priority: p,
        server: ServerId(s),
        item: v,
    };
    let items = vec![
        r(
            1,
            1,
            json!({"Name": "Big Buck Bunny", "Type": "Movie", "ProductionYear": 2008}),
        ),
        r(
            0,
            0,
            json!({"Name": "big buck bunny", "Type": "Movie", "ProductionYear": 2008}),
        ),
        r(
            2,
            2,
            json!({"Name": "Big Buck Bunny", "Type": "Movie", "ProductionYear": 2023}),
        ),
        r(
            2,
            2,
            json!({"Name": "Sintel", "Type": "Movie", "ProductionYear": 2010, "ProviderIds": {}}),
        ),
        r(
            0,
            0,
            json!({"Name": "Sintel", "Type": "Movie", "ProductionYear": 2010, "ProviderIds": {}}),
        ),
    ];
    let d = ProviderIdDeduplicator {
        types: vec!["Movie".into()],
        name_year_fallback: true,
    };
    let (out, stats) = d.dedupe(items.clone());
    assert_eq!(stats.removed, 1);
    assert_eq!(stats.drops[0].key, "title");
    assert_eq!(stats.drops[0].winner, ServerId(0));
    assert_eq!(out.len(), 4);
    let off = ProviderIdDeduplicator {
        types: vec!["Movie".into()],
        name_year_fallback: false,
    };
    assert_eq!(off.dedupe(items).1.removed, 0);
}

#[test]
fn views_grouping() {
    let a = vec![
        json!({"Name": "Movies", "CollectionType": "movies"}),
        json!({"Name": "Music", "CollectionType": "music"}),
    ];
    let b = vec![
        json!({"Name": "movies", "CollectionType": "movies"}),
        json!({"Name": "Shows", "CollectionType": "tvshows"}),
    ];
    let (s0, s1) = (ServerId(0), ServerId(1));
    let g = MergeByNameAndType.group_views(vec![(s0, a.clone()), (s1, b.clone())]);
    assert_eq!(g.len(), 3);
    assert!(matches!(&g[0], ViewGroup::Merged { members, .. } if members.len() == 2));
    assert!(matches!(&g[1], ViewGroup::Single(ServerId(0), _)));
    assert!(matches!(&g[2], ViewGroup::Single(ServerId(1), _)));
    let g = KeepSeparate.group_views(vec![(s0, a), (s1, b)]);
    assert_eq!(g.len(), 4);
}
