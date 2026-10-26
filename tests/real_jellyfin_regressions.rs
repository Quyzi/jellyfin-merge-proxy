//! Regressions found running the proxy against real Jellyfin 12.3 servers
//! (see scripts/real-jellyfin-test.sh). Each test reproduces the upstream
//! behaviour that broke the proxy with in-memory mock backends.

use jmp::backend::mock::{MockBackend, MockFactory};
use jmp::config::UpstreamConfig;
use jmp::{AppState, StateOptions};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;

const AUTH: &str =
    r#"MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="dev1", Version="10.10.7""#;

fn config(extra: &str) -> String {
    format!(
        r#"
[proxy]
listen = "127.0.0.1:0"
reload_interval_secs = 0

[[user]]
name = "me"
password = "pw"

[[server]]
name = "s1"
server = "http://s1:8096"
user = "u"
password = "p"

[[server]]
name = "s2"
server = "http://s2:8096"
user = "u"
password = "p"

[logging]
filter = "warn"
{extra}
"#
    )
}

struct Env {
    base: String,
    token: String,
    http: reqwest::Client,
    factory: Arc<MockFactory>,
    _data: tempfile::TempDir,
}

impl Env {
    async fn get(&self, path: &str) -> Value {
        let r = self
            .http
            .get(format!("{}/{path}", self.base))
            .header("Authorization", format!("{AUTH}, Token=\"{}\"", self.token))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "{path}: {}", r.status());
        r.json().await.unwrap()
    }
    fn mock(&self, i: usize) -> Arc<MockBackend> {
        self.factory.created()[i].clone()
    }
}

async fn start(
    extra: &str,
    setup: impl Fn(&MockBackend, &UpstreamConfig) + Send + Sync + 'static,
) -> Env {
    jmp::telemetry::init_metrics();
    let data = tempfile::tempdir().unwrap();
    let text = config(extra).replacen(
        "reload_interval_secs = 0",
        &format!(
            "reload_interval_secs = 0\ndata_dir = {:?}",
            data.path().display().to_string()
        ),
        1,
    );
    let cache = jmp::config::PasswordCache::default();
    let cfg = jmp::config::Config::parse(&text, &cache).unwrap();
    let factory = Arc::new(MockFactory::new(setup));
    let state = AppState::with_options(
        cfg,
        StateOptions {
            backend_factory: Some(factory.clone()),
            password_cache: Some(cache),
            ..Default::default()
        },
    )
    .unwrap();
    let app = jmp::build_app(state);
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
    let base = format!("http://{addr}");
    let http = reqwest::Client::new();
    let auth: Value = http
        .post(format!("{base}/Users/AuthenticateByName"))
        .header("Authorization", AUTH)
        .json(&json!({"Username": "me", "Pw": "pw"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    Env {
        base,
        token: auth["AccessToken"].as_str().unwrap().to_string(),
        http,
        factory,
        _data: data,
    }
}

fn id(parts: &[&str]) -> String {
    jmp::ids::hash_id(parts)
}

/// Real servers omit `ProviderIds` unless `Fields=ProviderIds` is sent, and
/// real clients rarely send it: the proxy must ask for it when deduping.
#[tokio::test]
async fn dedupe_requests_provider_ids_from_upstreams() {
    let env = start("[merge]\ndedupe = true\n", |m, cfg| {
        let bbb = json!({"Id": id(&["bbb", &cfg.name]), "Name": "Big Buck Bunny", "Type": "Movie",
                         "SortName": "big buck bunny", "ProviderIds": {"Tmdb": "10378"}});
        m.on_json(None, "Items/Latest", json!([bbb.clone()]));
        m.on_json(
            None,
            "Items",
            json!({"Items": [bbb], "TotalRecordCount": 1}),
        );
    })
    .await;
    let latest = env
        .get("Items/Latest?Limit=16&Fields=PrimaryImageAspectRatio")
        .await;
    assert_eq!(latest.as_array().unwrap().len(), 1, "{latest}");
    let items = env.get("Items?Recursive=true&IncludeItemTypes=Movie").await;
    assert_eq!(items["Items"].as_array().unwrap().len(), 1, "{items}");
    for i in 0..2 {
        let calls = env.mock(i).calls();
        let latest = calls
            .iter()
            .find(|c| c.path_and_query.starts_with("Items/Latest"))
            .unwrap();
        assert!(
            latest
                .path_and_query
                .contains("Fields=PrimaryImageAspectRatio%2CProviderIds")
                || latest
                    .path_and_query
                    .contains("Fields=PrimaryImageAspectRatio,ProviderIds"),
            "{}",
            latest.path_and_query
        );
        let items = calls
            .iter()
            .find(|c| c.path_and_query.starts_with("Items?"))
            .unwrap();
        assert!(
            items.path_and_query.contains("Fields=ProviderIds"),
            "{}",
            items.path_and_query
        );
    }
}

/// A show on two servers with different episodes (s1: S01E01-02, s2: S01E02-03
/// and S02E01). Dedupe used to hide the second copy and with it every episode
/// only that server has; now seasons and episodes merge by number.
#[tokio::test]
async fn series_split_across_servers_merges_seasons_and_episodes() {
    let env = start("[merge]\ndedupe = true\n", |m, cfg| {
        let n = cfg.name.as_str();
        let series = id(&["series", n]);
        let s1 = id(&["season1", n]);
        let s2 = id(&["season2", n]);
        let ep = |s: i64, e: i64, season: &str| {
            json!({"Id": id(&["ep", n, &s.to_string(), &e.to_string()]), "Type": "Episode",
                   "Name": format!("S{s}E{e} from {n}"), "ParentIndexNumber": s, "IndexNumber": e,
                   "SeriesId": series, "SeasonId": season})
        };
        m.on_json(
            None,
            "Items",
            json!({"Items": [{"Id": series, "Name": "Breaking Bad", "Type": "Series", "SortName": "breaking bad",
                              "ProductionYear": 2008, "ProviderIds": {"Tmdb": "1396"}}], "TotalRecordCount": 1}),
        );
        let season = |sid: &str, i: i64| json!({"Id": sid, "Type": "Season", "Name": format!("Season {i}"), "IndexNumber": i, "SeriesId": series});
        if n == "s1" {
            m.on_json(None, &format!("Shows/{series}/Seasons"), json!({"Items": [season(&s1, 1)], "TotalRecordCount": 1}));
            m.on_json(None, &format!("Shows/{series}/Episodes"), json!({"Items": [ep(1, 1, &s1), ep(1, 2, &s1)], "TotalRecordCount": 2}));
        } else {
            m.on_json(None, &format!("Shows/{series}/Seasons"), json!({"Items": [season(&s1, 1), season(&s2, 2)], "TotalRecordCount": 2}));
            m.on_json(None, &format!("Shows/{series}/Episodes"), json!({"Items": [ep(2, 1, &s2), ep(1, 2, &s1), ep(1, 3, &s1)], "TotalRecordCount": 3}));
        }
    })
    .await;
    let shows = env
        .get("Items?Recursive=true&IncludeItemTypes=Series")
        .await;
    let shows = shows["Items"].as_array().unwrap().clone();
    assert_eq!(shows.len(), 1, "series deduped: {shows:?}");
    let sid = shows[0]["Id"].as_str().unwrap().to_string();

    let seasons = env.get(&format!("Shows/{sid}/Seasons")).await;
    let names: Vec<&str> = seasons["Items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["Name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Season 1", "Season 2"], "{seasons}");
    assert_eq!(seasons["TotalRecordCount"], 2);

    let eps = env.get(&format!("Shows/{sid}/Episodes")).await;
    let names: Vec<&str> = eps["Items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["Name"].as_str().unwrap())
        .collect();
    // S01E02 exists on both: the priority winner (s1) is kept.
    assert_eq!(
        names,
        [
            "S1E1 from s1",
            "S1E2 from s1",
            "S1E3 from s2",
            "S2E1 from s2"
        ],
        "{eps}"
    );

    // The season ids handed out map back to each server's own season.
    let season1 = seasons["Items"][0]["Id"].as_str().unwrap();
    let _ = env
        .get(&format!("Shows/{sid}/Episodes?seasonId={season1}"))
        .await;
    for (i, n) in ["s1", "s2"].iter().enumerate() {
        let want = id(&["season1", n]);
        assert!(
            env.mock(i).calls().iter().any(|c| c
                .path_and_query
                .to_ascii_lowercase()
                .contains(&format!("seasonid={want}"))),
            "{n} got its own season id"
        );
    }
}

/// With a server down before its libraries were ever cached, every merged
/// library silently lost that server and no metric said the answer was
/// partial (the item fan-outs never even targeted it).
#[tokio::test]
async fn views_missing_a_down_server_count_as_partial() {
    let env = start("[metrics]\nenabled = true\n", |m, cfg| {
        m.on_json(
            None,
            "UserViews",
            json!({"Items": [{"Id": id(&["movies", &cfg.name]), "Name": "Movies", "CollectionType": "movies",
                              "Type": "CollectionFolder"}], "TotalRecordCount": 1}),
        );
    })
    .await;
    env.mock(1).set_down(true);
    let views = env.get("UserViews").await;
    assert_eq!(views["Items"].as_array().unwrap().len(), 1, "{views}");
    let m = env
        .http
        .get(format!("{}/metrics", env.base))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        m.lines()
            .any(|l| l.starts_with("jmp_fanout_partial_total{endpoint=\"views\"}")),
        "{m}"
    );
    assert!(
        m.lines()
            .any(|l| l.starts_with("jmp_fanout_upstream_results_total")
                && l.contains("endpoint=\"views\"")
                && l.contains("server=\"s2\"")
                && !l.contains("success")),
        "{m}"
    );
}
