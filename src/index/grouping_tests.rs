use super::*;
use std::collections::BTreeMap;

fn e(id: char, kind: &str, name: &str, prov: &[(&str, &str)]) -> Entry {
    Entry {
        id: id.to_string().repeat(32),
        kind: kind.into(),
        name: name.into(),
        providers: prov
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>(),
        ..Default::default()
    }
}

fn child(id: char, kind: &str, series: char, season: Option<i64>, n: i64) -> Entry {
    Entry {
        id: id.to_string().repeat(32),
        kind: kind.into(),
        series: Some(series.to_string().repeat(32)),
        parent_index: season,
        index: Some(n),
        ..Default::default()
    }
}

fn p(c: char) -> ProxyItemId {
    ProxyItemId::parse(&c.to_string().repeat(32)).unwrap()
}

fn policy() -> ProviderGrouping {
    ProviderGrouping {
        enabled: true,
        types: vec!["Movie".into(), "Series".into()],
    }
}

#[test]
fn groups_movies_and_split_shows() {
    let entries = [
        e('1', "Movie", "BBB", &[("tmdb", "10378")]),
        e(
            '2',
            "Movie",
            "Big Buck Bunny",
            &[("imdb", "x"), ("tmdb", "10378")],
        ),
        e('3', "Movie", "Sintel", &[]),
        e('4', "Movie", "sintel", &[]),
        e('5', "Series", "BB", &[("tvdb", "81189")]),
        e('6', "Series", "BB", &[("tvdb", "81189")]),
        child('7', "Season", '5', None, 1),
        child('8', "Season", '6', None, 1),
        child('9', "Season", '6', None, 2),
        child('a', "Episode", '5', Some(1), 1),
        child('b', "Episode", '6', Some(1), 3),
        child('c', "Episode", '6', Some(1), 1),
        e('d', "Movie", "Same server dup", &[("tmdb", "10378")]),
    ];
    let servers = [0, 1, 0, 1, 0, 1, 0, 1, 1, 0, 1, 1, 0];
    let copies: Vec<Copy> = entries
        .iter()
        .zip(servers)
        .map(|(entry, s)| Copy {
            server: ServerId(s),
            entry,
        })
        .collect();
    let g = policy().group(&copies);
    let find = |i: usize| g.iter().find(|x| x.contains(&i)).unwrap().clone();
    assert_eq!(find(0), vec![0, 1]);
    assert_eq!(find(2), vec![2, 3], "name+year fallback");
    assert_eq!(find(4), vec![4, 5]);
    assert_eq!(find(6), vec![6, 7], "season 1 merged");
    assert_eq!(find(8), vec![8]);
    assert_eq!(find(9), vec![9, 11], "S01E01 merged");
    assert_eq!(find(10), vec![10]);
    assert_eq!(find(12), vec![12], "same-server copy kept apart");
    let off = ProviderGrouping {
        enabled: false,
        ..policy()
    };
    assert_eq!(off.group(&copies).len(), copies.len());
}

#[test]
fn canonical_ids_are_stable() {
    // First build: best member wins.
    let (c, rows) = assign_canonical(&[vec![p('b'), p('a')], vec![p('c')]], &HashMap::new());
    assert_eq!(c, vec![p('b'), p('c')]);
    assert_eq!(rows.len(), 2, "singletons are not persisted: {rows:?}");
    let persisted: HashMap<_, _> = rows.into_iter().collect();
    // Ranking flips: the canonical id does not.
    let (c, rows) = assign_canonical(&[vec![p('a'), p('b')], vec![p('c')]], &persisted);
    assert_eq!(c[0], p('b'));
    assert!(rows.is_empty());
    // The canonical copy disappears: the group keeps its id.
    let (c, rows) = assign_canonical(&[vec![p('a')]], &persisted);
    assert_eq!(c, vec![p('b')]);
    assert!(rows.is_empty());
    // The group splits: the half holding the canonical copy keeps it.
    let (c, _) = assign_canonical(&[vec![p('a')], vec![p('b')]], &persisted);
    assert_eq!(c, vec![p('a'), p('b')]);
}
