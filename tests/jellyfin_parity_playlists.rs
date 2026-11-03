//! Jellyfin parity for playlists: the proxy owns them per user (mixing
//! items from several servers), upstream playlists keep being forwarded.

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
    factory: Arc<MockFactory>,
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
    fn mock(&self, i: usize) -> Arc<MockBackend> {
        self.factory.created()[i].clone()
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
        factory,
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

async fn login_as(http: &reqwest::Client, base: &str, user: &str) -> String {
    login(http, base, user, "pw", &format!("dev-{user}")).await
}

fn movie(server: &str, name: &str) -> Value {
    json!({"Id": id(&["movie", server, name]), "Name": name, "Type": "Movie",
           "SortName": name.to_lowercase(), "MediaType": "Video", "LocationType": "FileSystem",
           "RunTimeTicks": 6_000_000_000i64})
}

fn install(m: &MockBackend, cfg: &UpstreamConfig) {
    let n = cfg.name.clone();
    let mut all = vec![movie(&n, "A")];
    if n == "s2" {
        all.push(movie(&n, "B"));
    }
    let up_playlist = json!({"Id": id(&["playlist", &n]), "Name": format!("Up {n}"), "Type": "Playlist",
                             "SortName": format!("up {n}"), "MediaType": "Video", "IsFolder": true});
    m.on_json(
        None,
        &format!("Playlists/{}/Items", id(&["playlist", &n])),
        json!({"Items": [], "TotalRecordCount": 0}),
    );
    m.on_query(None, "Items", move |qs| {
        let pool: Vec<Value> = if q(qs, "IncludeItemTypes").is_some_and(|t| t == "Playlist") {
            vec![up_playlist.clone()]
        } else {
            all.clone()
        };
        let items: Vec<Value> = match q(qs, "Ids") {
            Some(ids) => {
                let want: Vec<&str> = ids.split(',').collect();
                pool.iter()
                    .chain(all.iter())
                    .filter(|v| want.contains(&v["Id"].as_str().unwrap_or("")))
                    .cloned()
                    .collect()
            }
            None => pool,
        };
        let total = items.len();
        json!({"Items": items, "TotalRecordCount": total, "StartIndex": 0})
    });
    m.on_json(
        None,
        "UserViews",
        json!({"Items": [{"Id": id(&["lib", &n]), "Name": "Movies", "Type": "CollectionFolder",
                          "CollectionType": "movies"}], "TotalRecordCount": 1}),
    );
}

fn ids(v: &Value) -> Vec<String> {
    v["Items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["Id"].as_str().unwrap().to_string())
        .collect()
}

/// A playlist mixing items from two servers: created, listed with entry ids,
/// reordered, trimmed, shown in the Playlists library, hidden from other
/// users when private, and upstream playlists still reach their own server.
#[tokio::test]
async fn playlists_mix_servers_and_stay_private() {
    let env = start("[[user]]\nname = \"you\"\npassword = \"pw\"\n", install).await;
    let movies = env
        .get("Items?Recursive=true&IncludeItemTypes=Movie&SortBy=SortName")
        .await;
    let both = ids(&movies);
    assert_eq!(both.len(), 3, "A twice (one per server) and B: {movies}");
    let b = movies["Items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["Name"] == "B")
        .unwrap()["Id"]
        .as_str()
        .unwrap()
        .to_string();
    let a = both.iter().find(|i| **i != b).unwrap().clone();

    let (s, v) = env
        .req(
            Method::POST,
            "Playlists",
            Some(json!({"Name": "Mix", "Ids": [a, b], "IsPublic": false})),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let pl = v["Id"].as_str().unwrap().to_string();

    let items = env.get(&format!("Playlists/{pl}/Items")).await;
    assert_eq!(ids(&items), [a.clone(), b.clone()], "{items}");
    for i in items["Items"].as_array().unwrap() {
        assert_eq!(i["PlaylistItemId"], i["Id"]);
    }

    let (s, _) = env
        .req(
            Method::POST,
            &format!("Playlists/{pl}/Items/{b}/Move/0"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let items = env.get(&format!("Playlists/{pl}/Items")).await;
    assert_eq!(ids(&items), [b.clone(), a.clone()], "{items}");

    let (s, _) = env
        .req(
            Method::DELETE,
            &format!("Playlists/{pl}/Items?entryIds={a}"),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let items = env.get(&format!("Playlists/{pl}/Items")).await;
    assert_eq!(ids(&items), std::slice::from_ref(&b), "{items}");

    let views = env.get("UserViews").await;
    let kinds: Vec<&str> = views["Items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["CollectionType"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["movies", "playlists"], "{views}");
    let listed = env.get("Items?IncludeItemTypes=Playlist").await;
    let names: Vec<&str> = listed["Items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["Name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Mix", "Up s1", "Up s2"], "{listed}");
    let one = env.get(&format!("Items/{pl}")).await;
    assert_eq!(
        (one["Type"].as_str(), one["ChildCount"].as_i64()),
        (Some("Playlist"), Some(1))
    );

    let other = login_as(&env.http, &env.base, "you").await;
    let r = env
        .http
        .get(format!("{}/Playlists/{pl}/Items", env.base))
        .header("Authorization", format!("{AUTH}, Token=\"{other}\""))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NOT_FOUND);
    assert_eq!(r.text().await.unwrap(), "Playlist not found");

    let upstream = listed["Items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["Name"] == "Up s2")
        .unwrap()["Id"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, _) = env
        .req(Method::GET, &format!("Playlists/{upstream}/Items"), None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert!(env
        .mock(1)
        .calls()
        .iter()
        .any(|c| c.path_and_query.starts_with("Playlists/")));
    assert!(!env
        .mock(0)
        .calls()
        .iter()
        .any(|c| c.path_and_query.starts_with("Playlists/")));
}
