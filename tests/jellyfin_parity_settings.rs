//! Parity with Jellyfin for user configuration, policy, password changes,
//! display preferences, the user list, admin-only areas and the image-info
//! list, all against in-memory mock backends.

use jmp::backend::mock::{MockBackend, MockFactory};
use jmp::config::UpstreamConfig;
use jmp::{AppState, StateOptions};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn auth(device: &str) -> String {
    format!(
        r#"MediaBrowser Client="Jellyfin Web", Device="Firefox", DeviceId="{device}", Version="10.10.7""#
    )
}

fn config(extra: &str, data: &Path) -> String {
    format!(
        r#"
[proxy]
listen = "127.0.0.1:0"
reload_interval_secs = 0
data_dir = {:?}

[[user]]
name = "me"
password = "pw"

[[user]]
name = "you"
password = "pw"

[[server]]
name = "s1"
server = "http://s1:8096"
user = "u"
password = "p"

[logging]
filter = "warn"
{extra}
"#,
        data.display().to_string()
    )
}

struct Env {
    base: String,
    token: String,
    http: reqwest::Client,
    factory: Arc<MockFactory>,
}

impl Env {
    async fn req(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        self.req_as(&self.token, method, path, body).await
    }

    async fn req_as(
        &self,
        token: &str,
        method: Method,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut b = self
            .http
            .request(method, format!("{}/{path}", self.base))
            .header(
                "Authorization",
                format!("{}, Token=\"{token}\"", auth("dev1")),
            );
        if let Some(body) = body {
            b = b.json(&body);
        }
        let r = b.send().await.unwrap();
        let status = r.status();
        let text = r.text().await.unwrap();
        let v = serde_json::from_str(&text).unwrap_or(Value::String(text));
        (status, v)
    }

    async fn login(&self, user: &str, pw: &str, device: &str) -> Option<String> {
        let r = self
            .http
            .post(format!("{}/Users/AuthenticateByName", self.base))
            .header("Authorization", auth(device))
            .json(&json!({"Username": user, "Pw": pw}))
            .send()
            .await
            .unwrap();
        if !r.status().is_success() {
            return None;
        }
        let v: Value = r.json().await.unwrap();
        Some(v["AccessToken"].as_str().unwrap().to_string())
    }

    fn mock(&self, i: usize) -> Arc<MockBackend> {
        self.factory.created()[i].clone()
    }
}

/// Boot a proxy over `data` (and optionally a config file to rewrite).
async fn boot(
    data: &Path,
    extra: &str,
    file: Option<PathBuf>,
    setup: impl Fn(&MockBackend, &UpstreamConfig) + Send + Sync + 'static,
) -> Env {
    jmp::telemetry::init_metrics();
    let text = config(extra, data);
    if let Some(f) = &file {
        std::fs::write(f, &text).unwrap();
    }
    let cache = jmp::config::PasswordCache::default();
    let cfg = jmp::config::Config::parse(&text, &cache).unwrap();
    let factory = Arc::new(MockFactory::new(setup));
    let state = AppState::with_options(
        cfg,
        StateOptions {
            backend_factory: Some(factory.clone()),
            password_cache: Some(cache),
            config_path: file,
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
    let mut env = Env {
        base: format!("http://{addr}"),
        token: String::new(),
        http: reqwest::Client::new(),
        factory,
    };
    env.token = env.login("me", "pw", "dev1").await.unwrap();
    env
}

async fn start() -> (Env, tempfile::TempDir) {
    let data = tempfile::tempdir().unwrap();
    (boot(data.path(), "", None, |_, _| {}).await, data)
}

const SERVER_ID: &str = "0123456789abcdef0123456789abcdef";

/// Posted configuration is replaced wholesale (unsent keys reset to their
/// defaults) and survives a restart on the same data directory; before this
/// the endpoint accepted and forgot everything.
#[tokio::test]
async fn user_configuration_round_trips_and_survives_restart() {
    let (env, data) = start().await;
    let (s, _) = env
        .req(
            Method::POST,
            "Users/Configuration",
            Some(json!({"DisplayMissingEpisodes": true, "OrderedViews": [SERVER_ID]})),
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let check = |me: Value| {
        let c = &me["Configuration"];
        assert_eq!(c["DisplayMissingEpisodes"], true, "{me}");
        assert_eq!(c["OrderedViews"], json!([SERVER_ID]));
        assert_eq!(c["HidePlayedInLatest"], true, "other keys stay default");
    };
    check(env.req(Method::GET, "Users/Me", None).await.1);
    let env = boot(data.path(), "", None, |_, _| {}).await;
    check(env.req(Method::GET, "Users/Me", None).await.1);
}

/// A user may only change their own configuration, and policy changes need
/// elevation nobody has; both used to answer 204.
#[tokio::test]
async fn configuration_and_policy_changes_are_restricted() {
    let (env, _d) = start().await;
    let me = env.req(Method::GET, "Users/Me", None).await.1;
    let you = env.req(Method::GET, "Users", None).await.1;
    let other = you
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["Name"] == "you")
        .unwrap()["Id"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, body) = env
        .req(
            Method::POST,
            &format!("Users/{other}/Configuration"),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(body, "User configuration update not allowed");
    let (s, _) = env
        .req(Method::POST, "Users/Configuration", Some(json!([1])))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let my_id = me["Id"].as_str().unwrap();
    let (s, _) = env
        .req(
            Method::POST,
            &format!("Users/{my_id}/Policy"),
            Some(json!({})),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, body) = env
        .req(Method::GET, &format!("Users/{SERVER_ID}"), None)
        .await;
    assert_eq!((s, body), (StatusCode::NOT_FOUND, json!("User not found")));
    let (s, v) = env.req(Method::GET, &format!("Users/{other}"), None).await;
    assert_eq!((s, &v["Name"]), (StatusCode::OK, &json!("you")));
}

/// Changing the password verifies the current one, rewrites the config
/// file, applies at once and revokes the user's other tokens.
#[tokio::test]
async fn password_change_rewrites_config_and_revokes_other_tokens() {
    let data = tempfile::tempdir().unwrap();
    let file = data.path().join("jmp.toml");
    let env = boot(data.path(), "", Some(file.clone()), |_, _| {}).await;
    let other = env.login("me", "pw", "dev2").await.unwrap();
    let (s, body) = env
        .req(
            Method::POST,
            "Users/Password",
            Some(json!({"CurrentPw": "nope", "NewPw": "fresh"})),
        )
        .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(body, "Invalid user or password entered.");
    let (s, _) = env
        .req(
            Method::POST,
            "Users/Password",
            Some(json!({"CurrentPw": "pw", "NewPw": "fresh"})),
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert!(env.login("me", "pw", "dev3").await.is_none());
    assert!(env.login("me", "fresh", "dev3").await.is_some());
    let (s, _) = env.req_as(&other, Method::GET, "Users/Me", None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "other tokens are revoked");
    let (s, _) = env.req(Method::GET, "Users/Me", None).await;
    assert_eq!(s, StatusCode::OK, "the changing session survives");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("$argon2"), "{text}");
    assert!(
        text.contains("name = \"you\""),
        "other users stay untouched"
    );
}

/// Display preferences use Jellyfin's derived guid and defaults, need a
/// client, and persist across a restart; they used to echo a constant.
#[tokio::test]
async fn display_preferences_follow_jellyfin_and_persist() {
    let (env, data) = start().await;
    let (s, _) = env
        .req(Method::GET, "DisplayPreferences/usersettings", None)
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = env
        .req(
            Method::GET,
            "DisplayPreferences/usersettings?client=emby",
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["Id"], "3ce5b65d-e116-d731-65d1-efc4a30ec35c");
    assert_eq!(v["CustomPrefs"]["skipForwardLength"], "30000");
    assert_eq!(v["CustomPrefs"]["enableNextVideoInfoOverlay"], "False");
    assert_eq!(v["Client"], "emby");
    let (s, _) = env
        .req(
            Method::POST,
            "DisplayPreferences/usersettings?client=emby",
            Some(json!({"CustomPrefs": {"homesection0": "nextup", "x": "1"}})),
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let env = boot(data.path(), "", None, |_, _| {}).await;
    let (_, v) = env
        .req(
            Method::GET,
            "DisplayPreferences/usersettings?client=emby",
            None,
        )
        .await;
    assert_eq!(v["CustomPrefs"]["homesection0"], "nextup");
    assert_eq!(v["CustomPrefs"]["x"], "1");
    assert_eq!(v["CustomPrefs"]["enableNextVideoInfoOverlay"], "True");
    let (_, other) = env
        .req(
            Method::GET,
            "DisplayPreferences/usersettings?client=other",
            None,
        )
        .await;
    assert_eq!(other["CustomPrefs"]["enableNextVideoInfoOverlay"], "False");
    let (s, _) = env
        .req(
            Method::POST,
            "DisplayPreferences/home?client=emby",
            Some(json!({"CustomPrefs": {"chromecastVersion": "bogus"}})),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

/// `GET /Users` lists every proxy user sorted by name; SyncPlay and the
/// admin areas are forbidden rather than answering empty lists.
#[tokio::test]
async fn user_list_and_admin_areas_match_jellyfin() {
    let (env, _d) = start().await;
    let (s, v) = env.req(Method::GET, "Users", None).await;
    assert_eq!(s, StatusCode::OK);
    let names: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["Name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["me", "you"]);
    let (_, v) = env.req(Method::GET, "Users?isHidden=false", None).await;
    assert_eq!(v, json!([]));
    let (s, _) = env.req(Method::POST, "SyncPlay/New", Some(json!({}))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    for p in ["Plugins", "Packages", "ScheduledTasks", "Repositories"] {
        let (s, _) = env.req(Method::GET, p, None).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{p}");
    }
    let (s, _) = env
        .http
        .get(format!("{}/Plugins/{SERVER_ID}/1.0/Image", env.base))
        .send()
        .await
        .map(|r| (r.status(), ()))
        .unwrap();
    assert_eq!(s, StatusCode::NOT_FOUND, "anonymous plugin image");
}

/// The image-info list needs a token (`[Authorize]`); image binaries stay
/// anonymous so `<img>` tags work.
#[tokio::test]
async fn image_info_list_requires_auth_but_binaries_do_not() {
    let data = tempfile::tempdir().unwrap();
    let env = boot(data.path(), "", None, |m, _| {
        m.on_json(
            None,
            "Items",
            json!({"Items": [{"Id": "m1", "Name": "M", "Type": "Movie"}], "TotalRecordCount": 1}),
        );
    })
    .await;
    let (_, items) = env.req(Method::GET, "Items?Recursive=true", None).await;
    let item = items["Items"][0]["Id"].as_str().unwrap().to_string();
    let anon = |p: String| {
        let r = env.http.get(format!("{}/{p}", env.base)).send();
        async move { r.await.unwrap().status() }
    };
    assert_eq!(
        anon(format!("Items/{item}/Images")).await,
        StatusCode::UNAUTHORIZED
    );
    let _ = anon(format!("Items/{item}/Images/Primary")).await;
    assert!(
        env.mock(0).calls().iter().any(|c| c
            .path_and_query
            .to_ascii_lowercase()
            .contains("/images/primary")),
        "the binary request reached the upstream"
    );
    let (s, _) = env
        .req(Method::GET, &format!("Items/{item}/Images"), None)
        .await;
    assert_ne!(s, StatusCode::UNAUTHORIZED, "with a token it is served");
}
