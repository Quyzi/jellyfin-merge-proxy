//! Jellyfin parity for the index and play-state items: virtual (missing)
//! episodes, `DisplayMissingEpisodes` and `DatePlayed`.

use jmp::backend::mock::{MockBackend, MockFactory};
use jmp::config::UpstreamConfig;
use jmp::{AppState, StateOptions};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

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
    state: Arc<AppState>,
    _data: tempfile::TempDir,
}

impl Env {
    async fn req(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut r = self
            .http
            .request(method, format!("{}/{path}", self.base))
            .header("Authorization", format!("{AUTH}, Token=\"{}\"", self.token));
        if let Some(b) = body {
            r = r.json(&b);
        }
        let r = r.send().await.unwrap();
        let status = r.status();
        let text = r.text().await.unwrap();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }
    async fn get(&self, path: &str) -> Value {
        let (s, v) = self.req(Method::GET, path, None).await;
        assert!(s.is_success(), "{path}: {s} {v}");
        v
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
    let state = AppState::with_options(
        cfg,
        StateOptions {
            backend_factory: Some(Arc::new(MockFactory::new(setup))),
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
    let base = format!("http://{addr}");
    let http = reqwest::Client::new();
    let token = login(&http, &base, "me", "pw", "dev1").await;
    Env {
        base,
        token,
        http,
        state,
        _data: data,
    }
}

async fn login(http: &reqwest::Client, base: &str, user: &str, pw: &str, device: &str) -> String {
    let auth = AUTH.replace("dev1", device);
    let v: Value = http
        .post(format!("{base}/Users/AuthenticateByName"))
        .header("Authorization", auth)
        .json(&json!({"Username": user, "Pw": pw}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    v["AccessToken"].as_str().unwrap().to_string()
}

fn id(parts: &[&str]) -> String {
    jmp::ids::hash_id(parts)
}

fn q<'a>(q: &'a [(String, String)], k: &str) -> Option<&'a str> {
    q.iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(k))
        .map(|(_, v)| v.as_str())
}

/// s1 holds a movie and a show whose first episode is virtual (missing);
/// s2 holds nothing.
fn install(m: &MockBackend, cfg: &UpstreamConfig) {
    let n = cfg.name.clone();
    let lib = id(&["lib", &n]);
    m.on_json(
        None,
        "UserViews",
        json!({"Items": [{"Id": lib, "Name": "All", "Type": "CollectionFolder",
                          "CollectionType": "movies"}], "TotalRecordCount": 1}),
    );
    let series = id(&["series", &n]);
    let season = id(&["season", &n]);
    let ep = |e: i64, location: &str| {
        json!({"Id": id(&["ep", &n, &e.to_string()]), "Name": format!("Episode {e}"), "Type": "Episode",
               "SeriesId": series, "SeriesName": "Show", "SeasonId": season, "ParentId": season,
               "ParentIndexNumber": 1, "IndexNumber": e, "RunTimeTicks": 6_000_000_000i64,
               "MediaType": "Video", "LocationType": location})
    };
    let all: Vec<Value> = if n == "s1" {
        vec![
            json!({"Id": id(&["movie", &n]), "Name": "Sintel", "Type": "Movie", "SortName": "sintel",
                   "RunTimeTicks": 6_000_000_000i64, "MediaType": "Video", "LocationType": "FileSystem"}),
            json!({"Id": series, "Name": "Show", "Type": "Series", "SortName": "show",
                   "ProviderIds": {"Tvdb": "1"}, "LocationType": "FileSystem"}),
            json!({"Id": season, "Name": "Season 1", "Type": "Season", "IndexNumber": 1,
                   "SeriesId": series, "ParentId": series, "LocationType": "FileSystem"}),
            ep(1, "Virtual"),
            ep(2, "FileSystem"),
        ]
    } else {
        vec![]
    };
    m.on_query(None, "Items", move |qs| {
        let items: Vec<Value> = match q(qs, "Ids") {
            Some(ids) => {
                let want: Vec<&str> = ids.split(',').collect();
                all.iter()
                    .filter(|v| want.contains(&v["Id"].as_str().unwrap_or("")))
                    .cloned()
                    .collect()
            }
            None => all
                .iter()
                .filter(|v| {
                    q(qs, "IncludeItemTypes").is_none_or(|t| t.split(',').any(|t| v["Type"] == t))
                })
                .cloned()
                .collect(),
        };
        let total = items.len();
        json!({"Items": items, "TotalRecordCount": total, "StartIndex": 0})
    });
}

fn names(v: &Value) -> Vec<&str> {
    v["Items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["Name"].as_str().unwrap())
        .collect()
}

/// Jellyfin hides missing episodes unless the user turned on
/// `DisplayMissingEpisodes`; the proxy listed them for everyone.
#[tokio::test]
async fn missing_episodes_are_hidden_until_the_user_opts_in() {
    let env = start("", install).await;
    let shows = env
        .get("Items?Recursive=true&IncludeItemTypes=Series")
        .await;
    let series = shows["Items"][0]["Id"].as_str().unwrap().to_string();
    let eps = env.get(&format!("Shows/{series}/Episodes")).await;
    assert_eq!(names(&eps), ["Episode 2"], "{eps}");
    let (s, only_missing) = env
        .req(
            Method::GET,
            &format!("Shows/{series}/Episodes?IsMissing=true"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(names(&only_missing).is_empty(), "{only_missing}");

    let me = env.get("Users/Me").await["Id"]
        .as_str()
        .unwrap()
        .to_string();
    env.state
        .local
        .settings
        .put(&me, &json!({"DisplayMissingEpisodes": true}))
        .unwrap();
    let eps = env.get(&format!("Shows/{series}/Episodes")).await;
    assert_eq!(names(&eps), ["Episode 1", "Episode 2"], "{eps}");
}

/// `DatePlayed` is parsed like Jellyfin's legacy date binder and stored in
/// the DTO timestamp format; garbage is a 400.
#[tokio::test]
async fn date_played_is_parsed_or_rejected() {
    let env = start("", install).await;
    let movies = env.get("Items?Recursive=true&IncludeItemTypes=Movie").await;
    let movie = movies["Items"][0]["Id"].as_str().unwrap().to_string();
    let (s, v) = env
        .req(
            Method::POST,
            &format!("UserPlayedItems/{movie}?DatePlayed=20261102153000"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["LastPlayedDate"], "2026-11-02T15:30:00.0000000Z", "{v}");
    assert_eq!(v["Played"], true);
    let (s, _) = env
        .req(
            Method::POST,
            &format!("UserPlayedItems/{movie}?DatePlayed=garbage"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}
