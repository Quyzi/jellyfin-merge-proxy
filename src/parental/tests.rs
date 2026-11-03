use super::filter::{apply, dto_allowed};
use super::*;
use crate::config::Config;
use crate::index::query::tests::snapshot;
use serde_json::json;
use std::sync::Arc;

fn find(snap: &Snapshot, name: &str) -> usize {
    (0..snap.items.len())
        .find(|i| snap.primary(&snap.items[*i]).name == name)
        .unwrap()
}

const PG: RatingPolicy = RatingPolicy {
    max: 10,
    block_unrated: false,
};

#[test]
fn scores_and_unrated() {
    let snap = snapshot();
    let (a, b, c) = (
        find(&snap, "Alpha"),
        find(&snap, "Bravo"),
        find(&snap, "Charlie"),
    );
    assert!(allows_item(&PG, &snap, a));
    assert!(!allows_item(&PG, &snap, b));
    assert!(allows_item(&PG, &snap, c), "unrated passes by default");
    let strict = RatingPolicy {
        block_unrated: true,
        ..PG
    };
    assert!(!allows_item(&strict, &snap, c));
    // Folders and libraries are never blocked as unrated.
    assert!(strict.allows(None, "CollectionFolder"));
    assert!(strict.allows(None, "MusicAlbum"));
}

#[test]
fn episodes_inherit_the_series_rating() {
    let mut snap = snapshot();
    let series = find(&snap, "Series10");
    let ep = find(&snap, "S1E1");
    assert!(allows_item(&PG, &snap, ep));
    snap.items[series].rating_score = Some(17);
    assert!(!allows_item(&PG, &snap, ep));
    assert!(!allows_item(&PG, &snap, find(&snap, "Season11")));
    // An episode's own rating wins.
    snap.items[ep].rating_score = Some(0);
    assert!(allows_item(&PG, &snap, ep));
}

#[test]
fn dtos_by_index_then_rating_name_then_series() {
    let mut snap = snapshot();
    let bravo = snap.items[find(&snap, "Bravo")].id.to_string();
    assert!(!dto_allowed(
        &PG,
        Some(&snap),
        &json!({"Id": bravo, "Type": "Movie"})
    ));
    let other = "f".repeat(32);
    let r = json!({"Id": other, "Type": "Movie", "OfficialRating": "R"});
    assert!(!dto_allowed(&PG, Some(&snap), &r));
    let pg = json!({"Id": other, "Type": "Movie", "OfficialRating": "pg"});
    assert!(dto_allowed(&PG, Some(&snap), &pg));
    let series = find(&snap, "Series10");
    snap.items[series].rating_score = Some(17);
    let sid = snap.items[series].id.to_string();
    let ep = json!({"Id": other, "Type": "Episode", "SeriesId": sid});
    assert!(!dto_allowed(&PG, Some(&snap), &ep));
    let mut page = json!({"Items": [r, pg, ep], "TotalRecordCount": 10});
    assert_eq!(apply(&PG, Some(&snap), &mut page), 2);
    assert_eq!(page["TotalRecordCount"], 8);
    assert_eq!(page["Items"].as_array().unwrap().len(), 1);
    let mut latest = json!([{"Id": bravo, "Type": "Movie"}, {"Type": "CollectionFolder"}]);
    assert_eq!(apply(&PG, Some(&snap), &mut latest), 1);
}

#[test]
fn service_resolves_names_and_reloads() {
    use crate::reload::{ReloadContext, Reloadable};
    let text = |limit: &str| {
        format!(
            "[[user]]\nname = \"kid\"\npassword = \"p\"\n{limit}\n\
             [[user]]\nname = \"adult\"\npassword = \"p\"\n\
             [[server]]\nname = \"a\"\nserver = \"http://127.0.0.1:1\"\napi_key = \"k\"\n"
        )
    };
    let cache = crate::config::PasswordCache::default();
    let cfg = Config::parse(&text("max_parental_rating = \"pg\""), &cache).unwrap();
    let store: Arc<dyn crate::store::Store> = Arc::new(crate::store::MemoryStore::new());
    let index = Arc::new(crate::index::service::IndexService::new(
        store,
        crate::servers::ServerRegistry::with_names(["a"]),
        Arc::new(crate::index::groups::CanonicalGroups::new()),
        100,
    ));
    let ids = Arc::new(crate::ids::HashedIdTable::new(
        crate::servers::ServerRegistry::with_names(["a"]),
    ));
    let svc = service::ParentalService::new(&cfg, index, ids);
    let kid = crate::users::user_id_for("kid");
    let adult = crate::users::user_id_for("adult");
    let snap = snapshot();
    assert_eq!(svc.policy(&kid, Some(&snap)).unwrap().max, 10);
    assert!(svc.policy(&adult, Some(&snap)).is_none());
    // Unknown names (or no index) fail closed.
    assert_eq!(svc.policy(&kid, None).unwrap().max, 0);
    let mut hits: Vec<usize> = (0..snap.items.len()).collect();
    svc.retain(&kid, &snap, &mut hits);
    assert!(!hits.contains(&find(&snap, "Bravo")));
    let new = Config::parse(&text("max_parental_rating = 17"), &cache).unwrap();
    let cx = ReloadContext {
        old: &cfg,
        new: &new,
        files_changed: false,
        tls: &crate::tls::RustlsProvider,
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    assert!(rt.block_on(svc.apply(&cx)).unwrap());
    assert_eq!(svc.policy(&kid, Some(&snap)).unwrap().max, 17);
}

#[test]
fn without_an_index_rated_videos_fail_closed() {
    let r = json!({"Id": "f".repeat(32), "Type": "Movie", "OfficialRating": "G"});
    let unrated = json!({"Id": "e".repeat(32), "Type": "Movie"});
    let folder = json!({"Type": "CollectionFolder", "OfficialRating": "G"});
    assert!(!dto_allowed(&PG, None, &r));
    assert!(dto_allowed(&PG, None, &unrated));
    assert!(dto_allowed(&PG, None, &folder));
    let mut page = vec![r, unrated, folder];
    assert_eq!(super::filter::retain_page(&PG, None, &mut page), 1);
}

/// Before the first index build a restricted user's items cannot be rated,
/// so `blocks` fails closed (as the list paths do) instead of letting
/// playback commands, playlists and item routes through; unrestricted users
/// are unaffected.
#[test]
fn without_an_index_item_checks_fail_closed() {
    let cache = crate::config::PasswordCache::default();
    let text = r#"
[proxy]
listen = "127.0.0.1:0"
[[user]]
name = "kid"
password = "pw"
max_parental_rating = "PG"
[[user]]
name = "adult"
password = "pw"
[[server]]
name = "s1"
server = "http://s1:8096"
user = "u"
password = "p"
"#;
    let cfg = Config::parse(text, &cache).unwrap();
    let reg = Arc::new(crate::servers::ServerRegistry::new());
    let index = Arc::new(crate::index::service::IndexService::new(
        Arc::new(crate::store::MemoryStore::new()),
        reg.clone(),
        Arc::new(crate::index::groups::CanonicalGroups::new()),
        100,
    ));
    let ids = Arc::new(crate::ids::HashedIdTable::new(reg));
    let svc = service::ParentalService::new(&cfg, index, ids);
    let id = crate::types::ProxyItemId::parse(&"a".repeat(32)).unwrap();
    assert!(svc.blocks(&crate::users::user_id_for("kid"), &id));
    assert!(!svc.blocks(&crate::users::user_id_for("adult"), &id));
}
