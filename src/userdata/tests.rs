use super::nextup::{next_up, NextUpQuery};
use super::rules::{self, ResumeRules};
use super::service::{aggregate, PlayEvent, UserDataService};
use super::view::UserDataOverlay;
use super::*;
use crate::config::UserDataConfig;
use crate::index::groups::CanonicalGroups;
use crate::index::service::IndexService;
use crate::index::snapshot::Snapshot;
use crate::servers::ServerRegistry;
use crate::store::{MemoryStore, Store};
use serde_json::json;
use std::sync::Arc;

const MIN: i64 = 60 * 10_000_000;

fn service(store: Arc<dyn Store>) -> UserDataService {
    let index = IndexService::new(
        store.clone(),
        ServerRegistry::with_names(["a"]),
        Arc::new(CanonicalGroups::new()),
        100,
    );
    UserDataService::new(
        Arc::new(UserDataTable::load(store)),
        Arc::new(index),
        &UserDataConfig::default(),
    )
}

fn pid(n: u32) -> ProxyItemId {
    ProxyItemId::parse(&format!("{n:032x}")).unwrap()
}

fn by_name(snap: &Snapshot, name: &str) -> usize {
    (0..snap.items.len())
        .find(|i| snap.primary(&snap.items[*i]).name == name)
        .unwrap()
}

#[test]
fn stopped_follows_jellyfin_thresholds() {
    let r = ResumeRules::default();
    let rt = Some(60 * MIN);
    let k = Some("Movie");
    let mut s = ItemState::default();
    assert!(
        !rules::stopped(&mut s, Some(MIN), rt, k, &r, false),
        "<5% resets"
    );
    assert_eq!(s.position_ticks, 0);
    assert!(!rules::stopped(&mut s, Some(30 * MIN), rt, k, &r, false));
    assert_eq!(s.position_ticks, 30 * MIN, "resume point kept");
    assert!(!s.played);
    assert!(
        rules::stopped(&mut s, Some(58 * MIN), rt, k, &r, false),
        ">90%"
    );
    assert!(s.played && s.position_ticks == 0);
    let mut short = ItemState::default();
    let clip = Some(MIN);
    assert!(rules::stopped(
        &mut short,
        Some(MIN / 2),
        clip,
        k,
        &r,
        false
    ));
    let mut start = ItemState::default();
    rules::start(&mut start, Some("Movie"), "now");
    assert_eq!(
        (start.play_count, start.last_played.as_deref()),
        (1, Some("now"))
    );
}

#[test]
fn update_from_dto_is_case_insensitive_and_partial() {
    let mut s = ItemState {
        favorite: true,
        ..Default::default()
    };
    rules::update_from(&mut s, &json!({"played": true, "PlaybackPositionTicks": 5}));
    assert!(s.played && s.favorite);
    assert_eq!(s.position_ticks, 5);
    rules::update_from(&mut s, &json!({"Likes": false, "IsFavorite": false}));
    assert_eq!((s.likes, s.favorite), (Some(false), false));
}

#[test]
fn users_are_independent_and_state_persists() {
    let store: Arc<dyn Store> = Arc::new(MemoryStore::new());
    let svc = service(store.clone());
    let movie = pid(1);
    svc.set_played("alice", &movie, true, None, "2026-01-01T00:00:00Z");
    svc.change("bob", &movie, "favorite", &|s| s.favorite = true);
    assert!(svc.own("alice", &movie).played);
    assert!(!svc.own("alice", &movie).favorite);
    assert!(!svc.own("bob", &movie).played);
    assert!(svc.own("bob", &movie).favorite);
    let reloaded = service(store);
    assert!(reloaded.own("alice", &movie).played, "survives restart");
    assert_eq!(reloaded.table().len(), 2);
}

#[test]
fn playback_without_known_runtime_keeps_position() {
    let svc = service(Arc::new(MemoryStore::new()));
    let item = pid(7);
    svc.playback("u", &item, PlayEvent::Start, None, false, "t1");
    svc.playback("u", &item, PlayEvent::Progress, Some(42), false, "t1");
    let s = svc.playback("u", &item, PlayEvent::Stopped, Some(99), false, "t2");
    assert_eq!((s.play_count, s.position_ticks, s.played), (1, 99, false));
    assert_eq!(PlayEvent::from_seg("Stopped"), Some(PlayEvent::Stopped));
    assert_eq!(PlayEvent::from_seg("ping"), None);
}

#[test]
fn seed_never_overwrites_local_state() {
    let table = UserDataTable::load(Arc::new(MemoryStore::new()));
    table.update("u", &[pid(1)], "favorite", &|s| s.favorite = true);
    let upstream = ItemState {
        played: true,
        ..Default::default()
    };
    let n = table.seed("u", vec![(pid(1), upstream.clone()), (pid(2), upstream)]);
    assert_eq!(n, 1);
    assert!(!table.get("u", &pid(1)).unwrap().played);
    assert!(table.get("u", &pid(2)).unwrap().played);
}

#[test]
fn split_show_has_one_next_up_across_servers() {
    let snap = crate::index::query::tests::snapshot();
    let table = UserDataTable::load(Arc::new(MemoryStore::new()));
    let s1e1 = snap.items[by_name(&snap, "S1E1")].id.clone();
    let s1e2 = snap.items[by_name(&snap, "S1E2")].id.clone();
    let s2e1 = by_name(&snap, "S2E1");
    assert_ne!(
        snap.copies[snap.items[s2e1].copies[0]].server,
        snap.copies[snap.items[by_name(&snap, "S1E1")].copies[0]].server,
        "fixture: the show is split across servers"
    );
    let state = |i: usize| table.get("u", &snap.items[i].id).unwrap_or_default();
    let q = NextUpQuery::default();
    assert!(
        next_up(&snap, &|_| true, &state, &q).is_empty(),
        "nothing played"
    );
    table.update("u", &[s1e1, s1e2], "played", &|s| {
        rules::mark_played(
            s,
            true,
            Some("2026-01-01T00:00:00Z"),
            "2026-01-01T00:00:00Z",
        )
    });
    let state = |i: usize| table.get("u", &snap.items[i].id).unwrap_or_default();
    assert_eq!(next_up(&snap, &|_| true, &state, &q), vec![s2e1]);
    let other = |i: usize| table.get("v", &snap.items[i].id).unwrap_or_default();
    assert!(next_up(&snap, &|_| true, &other, &q).is_empty(), "per user");
    let series = by_name(&snap, "Series10");
    let first = NextUpQuery {
        series: Some(series),
        ..Default::default()
    };
    let first_ep = by_name(&snap, "S1E1");
    assert_eq!(next_up(&snap, &|_| true, &other, &first), vec![first_ep]);
    let down = |s: crate::types::ServerId| s.0 == 0;
    assert!(next_up(&snap, &down, &state, &q).is_empty(), "offline copy");
}

#[test]
fn series_state_is_derived_from_its_episodes() {
    let snap = crate::index::query::tests::snapshot();
    let table = UserDataTable::load(Arc::new(MemoryStore::new()));
    let series = by_name(&snap, "Series10");
    let ep = |n: &str| snap.items[by_name(&snap, n)].id.clone();
    table.update("u", &[ep("S1E1")], "played", &|s| {
        rules::mark_played(s, true, Some("d1"), "d1")
    });
    let get = |k: &ProxyItemId| table.get("u", k);
    let (s, c) = aggregate(&snap, series, ItemState::default(), &get);
    let c = c.unwrap();
    assert_eq!((c.total, c.unplayed, s.played), (3, 2, false));
    table.update("u", &[ep("S1E2"), ep("S2E1")], "played", &|s| {
        rules::mark_played(s, true, Some("d2"), "d2")
    });
    let get = |k: &ProxyItemId| table.get("u", k);
    let (s, c) = aggregate(&snap, series, ItemState::default(), &get);
    assert!(s.played);
    assert_eq!(
        (c.unwrap().unplayed, s.last_played.as_deref()),
        (0, Some("d2"))
    );
}

#[test]
fn overlay_replaces_upstream_user_data() {
    let svc = Arc::new(service(Arc::new(MemoryStore::new())));
    svc.change("u", &pid(1), "favorite", &|s| s.favorite = true);
    let id = format!("{:032x}", 1);
    let mut v = json!({"Items": [{"Id": id, "RunTimeTicks": 100,
        "UserData": {"Played": true, "IsFavorite": false, "Key": "k"}}]});
    UserDataOverlay::new(svc).apply(&mut v, "u");
    let ud = &v["Items"][0]["UserData"];
    assert_eq!(ud["Played"], json!(false), "upstream state dropped");
    assert_eq!(ud["IsFavorite"], json!(true));
    assert_eq!(ud["Key"], json!("k"));
    assert_eq!(ud["ItemId"], json!(id));
}

/// Jellyfin's `MarkPlayed`: with no date the play count only rises to 1 and
/// the last played date is kept; unplayed clears the date.
#[test]
fn mark_played_follows_base_item_rules() {
    let mut s = ItemState {
        play_count: 3,
        last_played: Some("old".into()),
        ..Default::default()
    };
    rules::mark_played(&mut s, true, None, "now");
    assert_eq!((s.play_count, s.last_played.as_deref()), (3, Some("old")));
    rules::mark_played(&mut s, true, Some("dated"), "now");
    assert_eq!((s.play_count, s.last_played.as_deref()), (4, Some("dated")));
    rules::mark_played(&mut s, false, None, "now");
    assert_eq!(
        (s.play_count, s.last_played.clone(), s.played),
        (0, None, false)
    );
}

/// An indexed item with no known runtime is assumed fully played on
/// progress; a stop without a position counts a play; a failed stop and a
/// resumable start change nothing / only the count.
#[test]
fn play_state_edge_rules() {
    let r = ResumeRules::default();
    let mut s = ItemState::default();
    rules::progress(&mut s, Some(5), None, Some("Movie"), &r);
    assert!(s.played, "no runtime means played");
    let mut s = ItemState::default();
    assert!(rules::stopped(
        &mut s,
        None,
        Some(60 * MIN),
        Some("Movie"),
        &r,
        false
    ));
    assert_eq!(s.play_count, 1);
    let mut s = ItemState::default();
    rules::stopped(
        &mut s,
        Some(30 * MIN),
        Some(60 * MIN),
        Some("Movie"),
        &r,
        true,
    );
    assert_eq!(
        (s.play_count, s.position_ticks),
        (0, 0),
        "failed stop ignored"
    );
    let mut s = ItemState::default();
    rules::start(&mut s, Some("Audio"), "t");
    assert!(s.played, "audio has no resume point");
    rules::start(&mut s, Some("Movie"), "t");
    assert!(!s.played);
}
