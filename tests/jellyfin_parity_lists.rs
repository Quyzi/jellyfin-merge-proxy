//! Parity with Jellyfin for merged lists: upstream outages are errors, the
//! `Items/Latest` default limit, cold-index NextUp duplicates and the
//! client-name dependent DTO fields.

use jmp::backend::mock::{MockBackend, MockFactory};
use jmp::config::UpstreamConfig;
use jmp::{AppState, StateOptions};
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;

fn auth(client: &str, device: &str) -> String {
    format!(r#"MediaBrowser Client="{client}", Device="d", DeviceId="{device}", Version="1""#)
}

fn config(data_dir: &str, extra: &str) -> String {
    format!(
        r#"
[proxy]
listen = "127.0.0.1:0"
reload_interval_secs = 0
data_dir = {data_dir:?}

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
    async fn req(&self, path: &str) -> (StatusCode, Value) {
        let r = self
            .http
            .get(format!("{}/{path}", self.base))
            .header(
                "Authorization",
                format!("{}, Token=\"{}\"", auth("Web", "dev1"), self.token),
            )
            .send()
            .await
            .unwrap();
        let status = r.status();
        (status, r.json().await.unwrap_or(Value::Null))
    }
    fn mock(&self, i: usize) -> Arc<MockBackend> {
        self.factory.created()[i].clone()
    }
}

async fn login(http: &reqwest::Client, base: &str, client: &str) -> String {
    let v: Value = http
        .post(format!("{base}/Users/AuthenticateByName"))
        .header("Authorization", auth(client, "dev1"))
        .json(&json!({"Username": "me", "Pw": "pw"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    v["AccessToken"].as_str().unwrap().to_string()
}

async fn start(
    extra: &str,
    client: &str,
    setup: impl Fn(&MockBackend, &UpstreamConfig) + Send + Sync + 'static,
) -> Env {
    jmp::telemetry::init_metrics();
    let data = tempfile::tempdir().unwrap();
    let text = config(&data.path().display().to_string(), extra);
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
    let token = login(&http, &base, client).await;
    Env {
        base,
        token,
        http,
        factory,
        _data: data,
    }
}

fn id(parts: &[&str]) -> String {
    jmp::ids::hash_id(parts)
}

/// With every upstream down, a list or count used to be an empty 200, which
/// clients show as an empty library; Jellyfin has no such state, so it is a
/// 503. One server down still answers from the other.
#[tokio::test]
async fn all_upstreams_down_is_an_error_not_an_empty_library() {
    let env = start("[index]\nenabled = false\n", "Web", |m, cfg| {
        let item = json!({"Id": id(&["m", &cfg.name]), "Name": "M", "Type": "Movie"});
        m.on_json(
            None,
            "Items",
            json!({"Items": [item], "TotalRecordCount": 1}),
        );
        m.on_json(None, "Items/Counts", json!({"MovieCount": 1}));
    })
    .await;
    let (s, _) = env.req("Items?Recursive=true").await;
    assert_eq!(s, StatusCode::OK);
    env.mock(0).set_down(true);
    let (s, v) = env.req("Items?Recursive=true").await;
    assert_eq!(s, StatusCode::OK, "one server up still answers");
    assert_eq!(v["Items"].as_array().unwrap().len(), 1);
    env.mock(1).set_down(true);
    for path in ["Items?Recursive=true", "Items/Counts"] {
        let (s, _) = env.req(path).await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{path}");
    }
}

/// `Items/Latest` without `Limit` returns 20 items in Jellyfin; the merge of
/// two servers must not return 60.
#[tokio::test]
async fn latest_defaults_to_twenty_items() {
    let env = start("[index]\nenabled = false\n", "Web", |m, cfg| {
        let items: Vec<Value> = (0..30)
            .map(|i| {
                json!({"Id": id(&["l", &cfg.name, &i.to_string()]), "Name": format!("m{i}"),
                       "Type": "Movie", "DateCreated": format!("2026-01-{:02}T00:00:00Z", i + 1)})
            })
            .collect();
        m.on_json(None, "Items/Latest", Value::Array(items));
    })
    .await;
    let (s, v) = env.req("Items/Latest").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v.as_array().unwrap().len(), 20);
}

/// Without the index, a show split over two servers appeared twice in
/// NextUp; Jellyfin has one next-up episode per series.
#[tokio::test]
async fn next_up_has_one_item_per_series_without_the_index() {
    let env = start("[index]\nenabled = false\n", "Web", |m, cfg| {
        let ep = json!({"Id": id(&["ep", &cfg.name]), "Name": "E1", "Type": "Episode",
                        "SeriesName": "Show", "SeriesId": id(&["series", &cfg.name])});
        m.on_json(
            None,
            "Shows/NextUp",
            json!({"Items": [ep], "TotalRecordCount": 1}),
        );
    })
    .await;
    let (s, v) = env.req("Shows/NextUp").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["Items"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["TotalRecordCount"], 1);
}

/// Jellyfin adds child-count fields for some clients by name; the upstream
/// request must carry them so the DTOs have the counts.
#[tokio::test]
async fn kodi_clients_are_sent_child_counts() {
    let env = start("[index]\nenabled = false\n", "Kodi", |m, _| {
        m.on_json(None, "Items", json!({"Items": [], "TotalRecordCount": 0}));
    })
    .await;
    let (s, _) = env.req("Items?Fields=Path").await;
    assert_eq!(s, StatusCode::OK);
    let calls = env.mock(0).calls();
    let q = &calls
        .iter()
        .find(|c| c.path_and_query.starts_with("Items?"))
        .unwrap()
        .path_and_query;
    let decoded = q.replace("%2C", ",");
    assert!(
        decoded.contains("RecursiveItemCount") && decoded.contains("ChildCount"),
        "{q}"
    );
}
