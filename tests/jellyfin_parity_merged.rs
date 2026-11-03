//! Endpoints that used to reach only the primary server (Jellyfin parity
//! item 10): recommendations and suggestions here; name details, play-session
//! routing, QuickConnect and Live TV in `jellyfin_parity_merged/`.

#[path = "jellyfin_parity_merged/names.rs"]
mod names;
#[path = "jellyfin_parity_merged/sessions.rs"]
mod sessions;
#[path = "jellyfin_parity_merged/tv.rs"]
mod tv;

use jmp::backend::mock::{MockBackend, MockFactory};
use jmp::config::UpstreamConfig;
use jmp::{AppState, Shared, StateOptions};
use reqwest::{Method, StatusCode};
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
    state: Shared,
    _data: tempfile::TempDir,
}

impl Env {
    async fn get(&self, path: &str) -> Value {
        let (status, v) = self.req("GET", path, None).await;
        assert!(status.is_success(), "{path}: {status}");
        v
    }
    /// Any request as the logged-in user; does not assert success. A body
    /// that is not JSON comes back as a JSON string, an empty one as null.
    async fn req(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        self.send(method, path, body, true).await
    }
    async fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        token: bool,
    ) -> (StatusCode, Value) {
        let mut rb = self
            .http
            .request(method.parse().unwrap(), format!("{}/{path}", self.base));
        rb = rb.header(
            "Authorization",
            if token {
                format!("{AUTH}, Token=\"{}\"", self.token)
            } else {
                AUTH.to_string()
            },
        );
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let r = rb.send().await.unwrap();
        let status = r.status();
        let text = r.text().await.unwrap();
        let v = serde_json::from_str(&text).unwrap_or(if text.is_empty() {
            Value::Null
        } else {
            Value::String(text)
        });
        (status, v)
    }
    fn mock(&self, i: usize) -> Arc<MockBackend> {
        self.factory.created()[i].clone()
    }
    /// Upstream paths (with query) a mock was asked for with `method`.
    fn calls(&self, i: usize, method: Method) -> Vec<String> {
        self.mock(i)
            .calls()
            .into_iter()
            .filter(|c| c.method == method)
            .map(|c| c.path_and_query)
            .collect()
    }
}

/// Log `user` in on `device_id` and return the access token.
async fn login(base: &str, user: &str, pw: &str, device_id: &str) -> String {
    let auth = AUTH.replace("dev1", device_id);
    let v: Value = reqwest::Client::new()
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
    let base = format!("http://{addr}");
    let token = login(&base, "me", "pw", "dev1").await;
    Env {
        base,
        token,
        http: reqwest::Client::new(),
        factory,
        state,
        _data: data,
    }
}

fn id(parts: &[&str]) -> String {
    jmp::ids::hash_id(parts)
}

fn cat(t: &str, base: &str, items: &[(&str, &str)]) -> Value {
    json!({"RecommendationType": t, "BaselineItemName": base, "CategoryId": id(&["cat", base]),
           "Items": items.iter().map(|(i, n)| json!({"Id": i, "Name": n, "Type": "Movie"})).collect::<Vec<_>>()})
}

/// Movies/Recommendations only ever came from the primary server. Both
/// servers' categories must merge: the same baseline movie is one category
/// with the union of items, categories are ordered by recommendation type
/// and capped at five.
#[tokio::test]
async fn recommendations_merge_categories_across_servers() {
    let (a, b, c) = (id(&["m", "a"]), id(&["m", "b"]), id(&["m", "c"]));
    let env = start("", move |m, cfg| {
        let cats = if cfg.name == "s1" {
            json!([
                cat("HasLikedActor", "Zed", &[(&a, "A")]),
                cat("SimilarToRecentlyPlayed", "Heat", &[(&a, "A"), (&b, "B")]),
                cat("HasLikedDirector", "Mann", &[(&b, "B")]),
            ])
        } else {
            json!([
                cat("SimilarToRecentlyPlayed", "heat", &[(&c, "C")]),
                cat("SimilarToLikedItem", "Alien", &[(&c, "C")]),
                cat("HasActorFromRecentlyPlayed", "Pacino", &[(&c, "C")]),
                cat("HasDirectorFromRecentlyPlayed", "Scott", &[(&c, "C")]),
            ])
        };
        m.on_json(None, "Movies/Recommendations", cats);
    })
    .await;
    let v = env
        .get("Movies/Recommendations?categoryLimit=5&itemLimit=8")
        .await;
    let cats = v.as_array().unwrap();
    let types: Vec<&str> = cats
        .iter()
        .map(|c| c["RecommendationType"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        [
            "SimilarToRecentlyPlayed",
            "SimilarToLikedItem",
            "HasDirectorFromRecentlyPlayed",
            "HasActorFromRecentlyPlayed",
            "HasLikedDirector"
        ],
        "{v}"
    );
    assert_eq!(cats[0]["Items"].as_array().unwrap().len(), 3, "{v}");
    for i in 0..2 {
        let calls = env.calls(i, Method::GET);
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("Movies/Recommendations")),
            "s{} asked: {calls:?}",
            i + 1
        );
    }
}

/// With every server failing, recommendations used to look like "nothing
/// to recommend"; Jellyfin parity wants an outage to be an error.
#[tokio::test]
async fn recommendations_are_unavailable_when_every_server_fails() {
    let env = start("", |_, _| {}).await;
    let (status, _) = env.req("GET", "Movies/Recommendations", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

/// Items/Suggestions went to the primary only, and any merged page
/// reported the upstream totals summed. Both servers contribute, and
/// without EnableTotalRecordCount the total is the page size.
#[tokio::test]
async fn suggestions_merge_and_total_follows_the_page() {
    let env = start("", |m, cfg| {
        m.on_json(
            None,
            "Items/Suggestions",
            json!({"Items": [{"Id": id(&["sug", &cfg.name]), "Name": cfg.name.clone(), "Type": "Movie"}],
                   "TotalRecordCount": 99}),
        );
    })
    .await;
    let v = env.get("Items/Suggestions?type=Movie&limit=10").await;
    assert_eq!(v["Items"].as_array().unwrap().len(), 2, "{v}");
    assert_eq!(v["TotalRecordCount"], 2, "{v}");
    let v = env
        .get("Items/Suggestions?type=Movie&enableTotalRecordCount=true")
        .await;
    assert_eq!(v["TotalRecordCount"], 198, "{v}");
    let me = env.get("Users/Me").await["Id"]
        .as_str()
        .unwrap()
        .to_string();
    let v = env.get(&format!("Users/{me}/Suggestions")).await;
    assert_eq!(v["Items"].as_array().unwrap().len(), 2, "legacy route: {v}");
}
