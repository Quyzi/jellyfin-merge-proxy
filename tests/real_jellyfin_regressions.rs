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
