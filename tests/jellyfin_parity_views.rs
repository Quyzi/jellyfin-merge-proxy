//! Jellyfin parity for `UserViews`: external content, hidden views, the
//! user's own order, and the merged libraries' recursive item count.

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

fn views_of(v: &Value) -> Vec<&str> {
    v["Items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["Name"].as_str().unwrap())
        .collect()
}

fn view_id(v: &Value, name: &str) -> String {
    v["Items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["Name"] == name)
        .unwrap_or_else(|| panic!("no view {name}: {v}"))["Id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Libraries on s1 only (`Zeta`, `Alpha`, live TV); s2 has none.
fn install_views(m: &MockBackend, cfg: &UpstreamConfig) {
    let items = if cfg.name == "s1" {
        json!([
            {"Id": id(&["zeta"]), "Name": "Zeta", "Type": "CollectionFolder", "CollectionType": "movies"},
            {"Id": id(&["alpha"]), "Name": "Alpha", "Type": "CollectionFolder", "CollectionType": "tvshows"},
            {"Id": id(&["tv"]), "Name": "Live TV", "Type": "UserView", "CollectionType": "livetv"},
        ])
    } else {
        json!([])
    };
    let n = items.as_array().unwrap().len();
    m.on_json(
        None,
        "UserViews",
        json!({"Items": items, "TotalRecordCount": n}),
    );
    m.on_json(None, "Items", json!({"Items": [], "TotalRecordCount": 0}));
}

/// The proxy returned views in upstream order with live TV always present.
/// Jellyfin sorts them by name, drops external content on request, and puts
/// the user's `OrderedViews` first.
#[tokio::test]
async fn user_views_follow_external_content_and_the_users_order() {
    let env = start("", install_views).await;
    let v = env.get("UserViews").await;
    assert_eq!(views_of(&v), ["Alpha", "Live TV", "Zeta"], "{v}");
    let v = env.get("UserViews?includeExternalContent=false").await;
    assert_eq!(views_of(&v), ["Alpha", "Zeta"], "{v}");

    let all = env.get("UserViews").await;
    let me = env.get("Users/Me").await["Id"]
        .as_str()
        .unwrap()
        .to_string();
    let zeta = view_id(&all, "Zeta");
    let (s, _) = env
        .req(
            Method::POST,
            "Users/Configuration",
            Some(json!({"OrderedViews": [zeta.clone()], "MyMediaExcludes": [view_id(&all, "Alpha")]})),
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT, "{me}");
    let v = env.get("UserViews").await;
    assert_eq!(
        views_of(&v),
        ["Zeta", "Live TV"],
        "hidden view is dropped: {v}"
    );
    let v = env.get("UserViews?includeHidden=true").await;
    assert_eq!(views_of(&v), ["Zeta", "Alpha", "Live TV"], "{v}");
}

/// Two `Movies` libraries merge into one view whose `RecursiveItemCount`
/// is the number of distinct movies (Jellyfin counts items, and the copy of
/// one movie on both servers is one item), not the sum of the members.
#[tokio::test]
async fn merged_library_counts_distinct_items() {
    let env = start("[merge]\ndedupe = true\n", |m, cfg| {
        let n = cfg.name.clone();
        let lib = id(&["movies", &n]);
        m.on_json(
            None,
            "UserViews",
            json!({"Items": [{"Id": lib, "Name": "Movies", "Type": "CollectionFolder",
                              "CollectionType": "movies", "RecursiveItemCount": 2}],
                   "TotalRecordCount": 1}),
        );
        let movie = |name: &str, tmdb: &str| {
            json!({"Id": id(&["movie", &n, name]), "Name": name, "Type": "Movie",
                   "SortName": name.to_lowercase(), "ProviderIds": {"Tmdb": tmdb},
                   "RunTimeTicks": 6_000_000_000i64, "MediaType": "Video", "LocationType": "FileSystem"})
        };
        let all = if n == "s1" {
            vec![movie("A", "1"), movie("B", "2")]
        } else {
            vec![movie("A", "1"), movie("C", "3")]
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
                None => all.clone(),
            };
            let total = items.len();
            json!({"Items": items, "TotalRecordCount": total, "StartIndex": 0})
        });
    })
    .await;
    let v = env.get("UserViews").await;
    assert_eq!(views_of(&v), ["Movies"], "{v}");
    assert_eq!(v["Items"][0]["RecursiveItemCount"], 3, "{v}");
}
