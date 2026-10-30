//! Client-compatibility contract tests: the requests jellyfin-web, Android
//! TV and Swiftfin send, answered by the proxy over two fake upstreams that
//! honour `Ids`, `ParentId`, `IncludeItemTypes` and paging. The checks are
//! the response shapes and semantics Jellyfin clients rely on (exact
//! totals, deep paging, merged names, NextUp across a split show, per-user
//! state). The live counterpart is `scenario-compat.sh` in the real suite.

use jmp::backend::mock::MockFactory;
use jmp::{AppState, StateOptions};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::time::Duration;

const AUTH: &str =
    r#"MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="dev1", Version="10.10.7""#;

mod fake;
use fake::install;

struct Env {
    base: String,
    http: reqwest::Client,
    _data: tempfile::TempDir,
}

impl Env {
    async fn login(&self, user: &str) -> String {
        let v: Value = self
            .http
            .post(format!("{}/Users/AuthenticateByName", self.base))
            .header("Authorization", AUTH)
            .json(&json!({"Username": user, "Pw": "pw"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        v["AccessToken"].as_str().unwrap().to_string()
    }
    async fn call(&self, t: &str, post: bool, path: &str, body: Value) -> Value {
        let url = format!("{}/{path}", self.base);
        let r = if post {
            self.http.post(url).json(&body)
        } else {
            self.http.get(url)
        };
        let r = r
            .header("Authorization", format!("{AUTH}, Token=\"{t}\""))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "{path}: {}", r.status());
        r.json().await.unwrap_or(Value::Null)
    }
    async fn get(&self, t: &str, path: &str) -> Value {
        self.call(t, false, path, Value::Null).await
    }
    async fn post(&self, t: &str, path: &str, body: Value) {
        self.call(t, true, path, body).await;
    }
}

async fn start() -> Env {
    jmp::telemetry::init_metrics();
    let data = tempfile::tempdir().unwrap();
    let mut text = format!(
        "[proxy]\nlisten = \"127.0.0.1:0\"\nreload_interval_secs = 0\ndata_dir = {:?}\n",
        data.path().display().to_string()
    );
    for u in ["me", "other"] {
        text += &format!("[[user]]\nname = \"{u}\"\npassword = \"pw\"\n");
    }
    for s in ["s1", "s2"] {
        text += &format!(
            "[[server]]\nname = \"{s}\"\nserver = \"http://{s}:8096\"\nuser = \"u\"\npassword = \"p\"\n"
        );
    }
    text += "[merge]\ndedupe = true\n[userdata]\nseed_from_upstream = false\n[logging]\nfilter = \"warn\"\n";
    let cache = jmp::config::PasswordCache::default();
    let cfg = jmp::config::Config::parse(&text, &cache).unwrap();
    let state = AppState::with_options(
        cfg,
        StateOptions {
            backend_factory: Some(std::sync::Arc::new(MockFactory::new(install))),
            password_cache: Some(cache),
            ..Default::default()
        },
    )
    .unwrap();
    state.spawn_initial_logins();
    let app = jmp::build_app(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    for _ in 0..200 {
        if state
            .index
            .snapshot(&state.cfg(), &state.backends()[..])
            .is_some()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(state.index.latest().is_some(), "index never warmed");
    Env {
        base: format!("http://{addr}"),
        http: reqwest::Client::new(),
        _data: data,
    }
}

fn names(v: &Value) -> Vec<&str> {
    let list = v.get("Items").unwrap_or(v);
    list.as_array()
        .unwrap()
        .iter()
        .map(|i| i["Name"].as_str().unwrap())
        .collect()
}

const MOVIES: &str = "Items?Recursive=true&IncludeItemTypes=Movie";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn items_exact_totals_deep_paging_sorting_and_filters() {
    let env = start().await;
    let t = env.login("me").await;
    let all = env.get(&t, &format!("{MOVIES}&SortBy=SortName")).await;
    assert_eq!(
        names(&all),
        ["Big Buck Bunny", "Sintel", "Tears of Steel"],
        "{all}"
    );
    assert_eq!(all["TotalRecordCount"], 3);
    let page = env
        .get(
            &t,
            &format!("{MOVIES}&SortBy=SortName&StartIndex=2&Limit=1"),
        )
        .await;
    assert_eq!(
        (names(&page), &page["TotalRecordCount"]),
        (vec!["Tears of Steel"], &json!(3))
    );
    let desc = env
        .get(
            &t,
            &format!("{MOVIES}&SortBy=ProductionYear,SortName&SortOrder=Descending"),
        )
        .await;
    assert_eq!(names(&desc), ["Tears of Steel", "Sintel", "Big Buck Bunny"]);
    let anim = env
        .get(&t, &format!("{MOVIES}&SortBy=SortName&Genres=Animation"))
        .await;
    assert_eq!(
        (names(&anim), &anim["TotalRecordCount"]),
        (vec!["Big Buck Bunny", "Sintel"], &json!(2))
    );
    let persons = env.get(&t, "Persons?PersonTypes=Actor").await;
    assert_eq!(names(&persons), ["Actor A"], "one merged person: {persons}");
    let pid = persons["Items"][0]["Id"].as_str().unwrap();
    let by_person = env.get(&t, &format!("{MOVIES}&PersonIds={pid}")).await;
    assert_eq!(
        by_person["TotalRecordCount"], 3,
        "person id works on both servers"
    );
    let no_total = env
        .get(
            &t,
            &format!("{MOVIES}&EnableTotalRecordCount=false&Limit=1"),
        )
        .await;
    assert_eq!(names(&no_total).len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn genre_studio_person_dtos_and_filters_match_jellyfin() {
    let env = start().await;
    let t = env.login("me").await;
    let g = env.get(&t, "Genres?SortBy=SortName&Recursive=true&IncludeItemTypes=Movie&EnableTotalRecordCount=false").await;
    assert_eq!(
        names(&g),
        ["Animation", "Comedy", "Fantasy", "Science Fiction"]
    );
    assert_eq!(
        g["TotalRecordCount"], 0,
        "Jellyfin sends 0 when the total is disabled"
    );
    let anim = &g["Items"][0];
    assert_eq!(
        anim["ImageTags"]["Primary"], "nametag",
        "hydrated from upstream: {anim}"
    );
    assert!(
        anim.get("UserData").is_none(),
        "genres carry no UserData by default"
    );
    assert_eq!(
        (&anim["MovieCount"], &anim["ChildCount"]),
        (&json!(2), &json!(2)),
        "{anim}"
    );
    let s = env
        .get(&t, "Studios?Recursive=true&IncludeItemTypes=Series")
        .await;
    let amc = s["Items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["Name"] == "AMC")
        .unwrap();
    assert_eq!(
        (
            &amc["SeriesCount"],
            &amc["EpisodeCount"],
            &amc["ChildCount"]
        ),
        (&json!(1), &json!(3), &json!(4)),
        "{amc}"
    );
    assert!(amc.get("UserData").is_some(), "studios carry UserData");
    let p = env.get(&t, "Persons?Recursive=true").await;
    assert!(
        p["Items"][0].get("MovieCount").is_none(),
        "persons have no counts"
    );
    let f2 = env
        .get(&t, "Items/Filters2?Recursive=true&IncludeItemTypes=Movie")
        .await;
    assert_eq!(f2["Genres"].as_array().unwrap().len(), 4, "{f2}");
    assert_eq!(
        f2["AudioLanguages"].as_array().unwrap().len(),
        2,
        "languages from both servers: {f2}"
    );
    let f = env
        .get(&t, "Items/Filters?Recursive=true&IncludeItemTypes=Movie")
        .await;
    assert_eq!(f["Years"], json!([2008, 2010, 2012]));
    let y = env
        .get(
            &t,
            "Years?SortBy=SortName&SortOrder=Descending&Recursive=true",
        )
        .await;
    assert_eq!(
        names(&y),
        ["2012", "2010", "2008"],
        "years from both servers, once each"
    );
    let c = env.get(&t, "Items/Counts").await;
    let got: Vec<i64> = ["MovieCount", "SeriesCount", "EpisodeCount", "ItemCount"]
        .iter()
        .map(|k| c[*k].as_i64().unwrap())
        .collect();
    // 4 movies + 2 series copies + 3 episodes upstream; one movie and the
    // series are the same title on both servers.
    assert_eq!(got, [3, 1, 3, 15 - 2], "{c}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn split_show_nextup_and_folder_user_data() {
    let env = start().await;
    let t = env.login("me").await;
    let shows = env
        .get(&t, "Items?Recursive=true&IncludeItemTypes=Series")
        .await;
    assert_eq!(names(&shows), ["Show"]);
    let series = shows["Items"][0]["Id"].as_str().unwrap().to_string();
    let ud = &shows["Items"][0]["UserData"];
    assert_eq!(
        (&ud["PlayedPercentage"], &ud["UnplayedItemCount"]),
        (&json!(0.0), &json!(3)),
        "{ud}"
    );
    let eps = env.get(&t, &format!("Shows/{series}/Episodes")).await;
    assert_eq!(
        names(&eps),
        ["Episode 1", "Episode 2", "Episode 3"],
        "episodes from both servers"
    );
    let e1 = eps["Items"][0]["Id"].as_str().unwrap();
    let e2 = eps["Items"][1]["Id"].as_str().unwrap().to_string();
    env.post(&t, &format!("UserPlayedItems/{e1}"), Value::Null)
        .await;
    env.post(&t, &format!("UserPlayedItems/{e2}"), Value::Null)
        .await;
    let next = env
        .get(&t, "Shows/NextUp?Limit=24&EnableTotalRecordCount=false")
        .await;
    assert_eq!(
        names(&next),
        ["Episode 3"],
        "NextUp crosses servers: {next}"
    );
    let s = env.get(&t, &format!("Items?Ids={series}")).await;
    let s = &s["Items"][0];
    let pct = s["UserData"]["PlayedPercentage"].as_f64().unwrap();
    assert!((pct - 200.0 / 3.0).abs() < 1e-6, "{}", s["UserData"]);
    let other = env.login("other").await;
    let next = env.get(&other, "Shows/NextUp?Limit=24").await;
    assert_eq!(
        names(&next),
        Vec::<&str>::new(),
        "the other user has watched nothing"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_favourites_latest_and_hints_per_user() {
    let env = start().await;
    let (me, other) = (env.login("me").await, env.login("other").await);
    let all = env.get(&me, &format!("{MOVIES}&SortBy=SortName")).await;
    let sintel = all["Items"][1]["Id"].as_str().unwrap().to_string();
    for e in ["Playing", "Playing/Progress", "Playing/Stopped"] {
        let body =
            json!({"ItemId": sintel, "MediaSourceId": sintel, "PositionTicks": 1_200_000_000i64});
        env.post(&me, &format!("Sessions/{e}"), body).await;
    }
    env.post(&me, &format!("UserFavoriteItems/{sintel}"), Value::Null)
        .await;
    let resume = env
        .get(&me, "UserItems/Resume?MediaTypes=Video&Limit=12")
        .await;
    assert_eq!(names(&resume), ["Sintel"], "{resume}");
    assert_eq!(
        resume["Items"][0]["UserData"]["PlaybackPositionTicks"],
        1_200_000_000i64
    );
    assert_eq!(
        names(&env.get(&other, "UserItems/Resume?MediaTypes=Video").await),
        Vec::<&str>::new()
    );
    let fav = env.get(&me, &format!("{MOVIES}&Filters=IsFavorite")).await;
    assert_eq!(names(&fav), ["Sintel"]);
    let fav = env
        .get(&other, &format!("{MOVIES}&Filters=IsFavorite"))
        .await;
    assert_eq!(fav["TotalRecordCount"], 0);
    let ud = env.get(&me, &format!("UserItems/{sintel}/UserData")).await;
    assert_eq!(
        (&ud["IsFavorite"], &ud["ItemId"]),
        (&json!(true), &json!(sintel)),
        "{ud}"
    );
    let latest = env
        .get(&me, "Items/Latest?Limit=16&IncludeItemTypes=Movie")
        .await;
    let latest = names(&latest);
    assert_eq!(
        latest.iter().filter(|n| **n == "Big Buck Bunny").count(),
        1,
        "deduped: {latest:?}"
    );
    let hits = env
        .get(
            &me,
            "Items?searchTerm=big&Recursive=true&IncludeItemTypes=Movie,Series",
        )
        .await;
    assert_eq!(names(&hits), ["Big Buck Bunny"]);
}
