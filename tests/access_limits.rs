//! Concurrent session caps and optional country allowlists.

use jmp::backend::mock::{MockBackend, MockFactory};
use jmp::config::UpstreamConfig;
use jmp::{AppState, StateOptions};
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;

fn auth(device: &str) -> String {
    format!(r#"MediaBrowser Client="t", Device="d", DeviceId="{device}", Version="1""#)
}

fn toml(user_extra: &str, geo: &str) -> String {
    format!(
        r#"
[proxy]
listen = "127.0.0.1:0"
reload_interval_secs = 0

[[user]]
name = "me"
password = "pw"
{user_extra}

[[server]]
name = "s1"
server = "http://s1:8096"
user = "u"
password = "p"

[index]
enabled = false

[logging]
filter = "warn"
{geo}
"#
    )
}

struct Env {
    base: String,
    http: reqwest::Client,
    _data: tempfile::TempDir,
}

impl Env {
    async fn boot(user_extra: &str, geo: &str) -> Self {
        jmp::telemetry::init_metrics();
        let data = tempfile::tempdir().unwrap();
        let text = toml(user_extra, geo).replacen(
            "reload_interval_secs = 0",
            &format!(
                "reload_interval_secs = 0\ndata_dir = {:?}",
                data.path().display().to_string()
            ),
            1,
        );
        let cache = jmp::config::PasswordCache::default();
        let cfg = jmp::config::Config::parse(&text, &cache).unwrap();
        let factory = Arc::new(MockFactory::new(|m: &MockBackend, _: &UpstreamConfig| {
            let lib = json!({"Items": [], "TotalRecordCount": 0});
            m.on_json(None, "UserViews", lib.clone());
            m.on_json(None, &format!("Users/{}/Views", m.user_id()), lib);
        }));
        let state = AppState::with_options(
            cfg,
            StateOptions {
                backend_factory: Some(factory),
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
            http: reqwest::Client::new(),
            _data: data,
        }
    }

    async fn login(
        &self,
        device: &str,
        country: Option<&str>,
    ) -> (StatusCode, Option<String>, Option<Value>) {
        let mut r = self
            .http
            .post(format!("{}/Users/AuthenticateByName", self.base))
            .header("Authorization", auth(device))
            .json(&json!({"Username": "me", "Pw": "pw"}));
        if let Some(c) = country {
            r = r.header("CF-IPCountry", c);
        }
        let resp = r.send().await.unwrap();
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::String(text.clone()));
        let token = v["AccessToken"].as_str().map(str::to_string);
        (status, token, Some(v))
    }

    async fn get(&self, token: &str, device: &str, country: Option<&str>) -> StatusCode {
        let mut r = self.http.get(format!("{}/Users/Me", self.base)).header(
            "Authorization",
            format!("{}, Token=\"{token}\"", auth(device)),
        );
        if let Some(c) = country {
            r = r.header("CF-IPCountry", c);
        }
        r.send().await.unwrap().status()
    }
}

/// max_active_sessions refuses a third distinct device; same device replaces.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_cap_blocks_extra_device_and_allows_same_device() {
    let env = Env::boot("max_active_sessions = 2", "").await;
    assert_eq!(env.login("devA", None).await.0, StatusCode::OK);
    assert_eq!(env.login("devB", None).await.0, StatusCode::OK);
    let (st, _, body) = env.login("devC", None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "{body:?}");
    // Replacing an existing device still works.
    assert_eq!(env.login("devA", None).await.0, StatusCode::OK);
}

/// Policy DTO reflects the configured cap.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn user_policy_reports_max_active_sessions() {
    let env = Env::boot("max_active_sessions = 3", "").await;
    let (st, token, body) = env.login("dev1", None).await;
    assert_eq!(st, StatusCode::OK, "{body:?}");
    let token = token.unwrap();
    let resp = env
        .http
        .get(format!("{}/Users/Me", env.base))
        .header(
            "Authorization",
            format!("{}, Token=\"{token}\"", auth("dev1")),
        )
        .send()
        .await
        .unwrap();
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["Policy"]["MaxActiveSessions"], 3);
}

/// allowed_countries + [geo] gate login and authenticated requests.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn geo_allowlist_fail_closed() {
    let env = Env::boot(
        r#"allowed_countries = ["US", "CA"]"#,
        r#"[geo]
country_header = "CF-IPCountry"
"#,
    )
    .await;
    assert_eq!(env.login("dev1", None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(env.login("dev1", Some("DE")).await.0, StatusCode::FORBIDDEN);
    let (st, token, _) = env.login("dev1", Some("us")).await;
    assert_eq!(st, StatusCode::OK);
    let token = token.unwrap();
    assert_eq!(env.get(&token, "dev1", Some("US")).await, StatusCode::OK);
    assert_eq!(
        env.get(&token, "dev1", Some("FR")).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(env.get(&token, "dev1", None).await, StatusCode::FORBIDDEN);
}

#[test]
fn geo_config_required_when_allowlist_set() {
    let cache = jmp::config::PasswordCache::default();
    let bad = r#"
[[user]]
name = "me"
password = "pw"
allowed_countries = ["US"]
[[server]]
name = "s"
server = "http://s:8096"
api_key = "k"
"#;
    assert!(jmp::config::Config::parse(bad, &cache).is_err());
    let ok = format!("{bad}\n[geo]\ncountry_header = \"CF-IPCountry\"\n");
    let cfg = jmp::config::Config::parse(&ok, &cache).unwrap();
    assert_eq!(cfg.users[0].allowed_countries, vec!["US"]);
}
