//! Name-detail routes (`Genres/{name}`, `Persons/{name}`, ...).

use super::*;

/// Genres/Studios/Persons/Artists/MusicGenres detail routes by name went to
/// the primary server. With a cold index they probe: the server that has
/// the person answers (and only it gets the real request).
#[tokio::test]
async fn person_detail_is_answered_by_the_server_that_has_the_name() {
    let env = start("", |m, cfg| {
        if cfg.name == "s2" {
            m.on_json(
                None,
                "Persons/Jane%20Doe",
                json!({"Name": "Jane Doe", "Id": id(&["jane"]), "Type": "Person"}),
            );
        }
    })
    .await;
    let v = env.get("Persons/Jane%20Doe").await;
    assert_eq!(v["Name"], "Jane Doe", "{v}");
    let s1 = env.calls(0, Method::GET);
    let s2 = env.calls(1, Method::GET);
    let hits = |c: &[String]| c.iter().filter(|p| p.starts_with("Persons/Jane")).count();
    assert_eq!(hits(&s1), 1, "s1 only probed: {s1:?}");
    assert_eq!(hits(&s2), 2, "s2 probed and answered: {s2:?}");
}

/// Genres/Studios/Artists are probed with an Items filter (the name route
/// itself would create the item upstream) and then forwarded to the first
/// server with matches.
#[tokio::test]
async fn genre_detail_probes_with_a_filter_not_the_name_route() {
    let env = start("", |m, cfg| {
        if cfg.name == "s2" {
            m.on_query(None, "Items", |q| {
                let has = q.iter().any(|(k, v)| k == "Genres" && v == "Drama");
                json!({"Items": [], "TotalRecordCount": if has { 3 } else { 0 }})
            });
            m.on_json(
                None,
                "Genres/Drama",
                json!({"Name": "Drama", "Type": "Genre"}),
            );
        } else {
            m.on_json(None, "Items", json!({"Items": [], "TotalRecordCount": 0}));
        }
    })
    .await;
    let v = env.get("Genres/Drama").await;
    assert_eq!(v["Name"], "Drama", "{v}");
    let s1 = env.calls(0, Method::GET);
    assert!(
        !s1.iter().any(|p| p.starts_with("Genres/")),
        "s1 must not see the name route: {s1:?}"
    );
    assert!(s1
        .iter()
        .any(|p| p.starts_with("Items?") && p.contains("Genres=Drama")));
}

/// With a warm index the name's owner is known without probing anyone.
#[tokio::test]
async fn person_detail_uses_the_index_when_warm() {
    let env = start("", |m, cfg| {
        let lib = id(&["lib", &cfg.name]);
        m.on_json(
            None,
            "UserViews",
            json!({"Items": [{"Id": lib, "Name": "Movies", "CollectionType": "movies", "Type": "CollectionFolder"}],
                   "TotalRecordCount": 1}),
        );
        let people = if cfg.name == "s2" {
            json!([{"Name": "Zed Person", "Id": id(&["zed"]), "Type": "Actor"}])
        } else {
            json!([])
        };
        m.on_json(
            None,
            "Items",
            json!({"Items": [{"Id": id(&["film", &cfg.name]), "Name": "Film", "Type": "Movie", "People": people,
                              "SortName": "film"}], "TotalRecordCount": 1}),
        );
        if cfg.name == "s2" {
            m.on_json(None, "Persons/Zed%20Person", json!({"Name": "Zed Person", "Type": "Person"}));
        }
    })
    .await;
    let set = env.state.backends();
    env.state.index.crawl_all(&set[..]).await;
    env.state
        .index
        .rebuild(&env.state.cfg(), &set[..], env.state.ids.as_ref());
    let v = env.get("Persons/Zed%20Person").await;
    assert_eq!(v["Name"], "Zed Person", "{v}");
    let s1 = env.calls(0, Method::GET);
    assert!(
        !s1.iter().any(|p| p.starts_with("Persons/")),
        "the index named s2 as owner: {s1:?}"
    );
}
