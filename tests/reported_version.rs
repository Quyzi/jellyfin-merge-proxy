//! Omitted / empty `[proxy] reported_version` reports the highest upstream version.

use jmp::backend::mock::{MockBackend, MockFactory};
use jmp::backend::MediaBackend;
use jmp::{AppState, StateOptions};
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;

const CONFIG: &str = r#"
[proxy]
listen = "127.0.0.1:0"
name = "Version Proxy"
reload_interval_secs = 0

[[user]]
name = "me"
password = "pw"

[[server]]
name = "old"
server = "http://old:8096"
api_key = "k"
user = "u"

[[server]]
name = "new"
server = "http://new:8096"
api_key = "k"
user = "u"

[logging]
filter = "warn"
"#;

struct Env {
    base: String,
    factory: Arc<MockFactory>,
}

async fn start(proxy_extra: &str) -> Env {
    let data = tempfile::tempdir().unwrap();
    let text = CONFIG.replacen(
        "reload_interval_secs = 0",
        &format!(
            "reload_interval_secs = 0\ndata_dir = {:?}\n{proxy_extra}",
            data.path().display().to_string()
        ),
        1,
    );
    let cache = jmp::config::PasswordCache::default();
    let cfg = jmp::config::Config::parse(&text, &cache).unwrap();
    let factory = Arc::new(MockFactory::new(|m: &MockBackend, cfg| {
        match cfg.name.as_str() {
            "old" => m.set_version(Some("10.10.7")),
            "new" => m.set_version(Some("10.11.5")),
            _ => {}
        }
    }));
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
    Env {
        base: format!("http://{addr}"),
        factory,
    }
}

async fn public_version(base: &str) -> String {
    let info: Value = reqwest::Client::new()
        .get(format!("{base}/System/Info/Public"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    info["Version"].as_str().unwrap().to_string()
}

/// Omitting reported_version → clients see max cached upstream version.
#[tokio::test]
async fn omitted_reported_version_uses_highest_upstream() {
    let env = start("").await;
    assert_eq!(public_version(&env.base).await, "10.11.5");
}

/// After upstreams stop advertising a version, keep the last live max.
#[tokio::test]
async fn blank_keeps_last_max_when_live_versions_vanish() {
    let env = start(r#"reported_version = """#).await;
    assert_eq!(public_version(&env.base).await, "10.11.5");
    for m in env.factory.created() {
        m.set_version(None);
    }
    assert_eq!(public_version(&env.base).await, "10.11.5");
}

/// Live max replaces a higher sticky when an upstream is still reporting.
#[tokio::test]
async fn live_max_drops_when_highest_upstream_downgrades() {
    let env = start("").await;
    assert_eq!(public_version(&env.base).await, "10.11.5");
    for m in env.factory.created() {
        match m.name() {
            "new" => m.set_version(None),
            "old" => m.set_version(Some("10.10.7")),
            _ => {}
        }
    }
    assert_eq!(public_version(&env.base).await, "10.10.7");
}

/// Explicit pin still overrides upstreams.
#[tokio::test]
async fn pinned_reported_version_wins() {
    let env = start(r#"reported_version = "10.9.11""#).await;
    assert_eq!(public_version(&env.base).await, "10.9.11");
}
