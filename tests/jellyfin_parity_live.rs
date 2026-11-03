//! Live sessions, WebSocket push and remote control (parity plan item 1).

use futures_util::{SinkExt, StreamExt};
use jmp::backend::mock::{MockBackend, MockFactory};
use jmp::config::UpstreamConfig;
use jmp::{AppState, StateOptions};
use reqwest::StatusCode;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

fn auth(device: &str) -> String {
    format!(r#"MediaBrowser Client="t", Device="d", DeviceId="{device}", Version="1""#)
}

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
    state: jmp::Shared,
    addr: SocketAddr,
    base: String,
    http: reqwest::Client,
    _data: tempfile::TempDir,
}

struct Login {
    token: String,
    session_id: String,
    user_id: String,
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

impl Env {
    async fn req(
        &self,
        token: &str,
        device: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
        let mut r = self
            .http
            .request(m, format!("{}/{path}", self.base))
            .header(
                "Authorization",
                format!("{}, Token=\"{token}\"", auth(device)),
            );
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().await.unwrap();
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        (
            status,
            serde_json::from_str(&text).unwrap_or(Value::String(text)),
        )
    }

    async fn login(&self, user: &str, pw: &str, device: &str) -> Login {
        let v: Value = self
            .http
            .post(format!("{}/Users/AuthenticateByName", self.base))
            .header("Authorization", auth(device))
            .json(&json!({"Username": user, "Pw": pw}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        Login {
            token: v["AccessToken"].as_str().unwrap().into(),
            session_id: v["SessionInfo"]["Id"].as_str().unwrap().into(),
            user_id: v["User"]["Id"].as_str().unwrap().into(),
        }
    }

    /// Connect a socket and wait for the greeting, so the server has
    /// attached it.
    async fn socket(&self, token: &str) -> Ws {
        let url = format!("ws://{}/socket?api_key={token}", self.addr);
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let hello = recv_kind(&mut ws, "ForceKeepAlive").await;
        assert_eq!(hello["Data"], 60, "{hello}");
        ws
    }
}

async fn recv_kind(ws: &mut Ws, kind: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => {
                    let v: Value = serde_json::from_str(t.as_str()).unwrap();
                    if v["MessageType"] == kind {
                        return v;
                    }
                }
                Some(Ok(_)) => {}
                other => panic!("socket ended while waiting for {kind}: {other:?}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {kind} message within 5 s"))
}

async fn send(ws: &mut Ws, v: Value) {
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}

async fn start(extra: &str) -> (Env, Arc<MockFactory>) {
    jmp::telemetry::init_metrics();
    let data = tempfile::tempdir().unwrap();
    let extra = if extra.contains("[index]") {
        extra.to_string()
    } else {
        format!("[index]\nenabled = false\n{extra}")
    };
    let text = config(&extra).replacen(
        "reload_interval_secs = 0",
        &format!(
            "reload_interval_secs = 0\ndata_dir = {:?}",
            data.path().display().to_string()
        ),
        1,
    );
    let cache = jmp::config::PasswordCache::default();
    let cfg = jmp::config::Config::parse(&text, &cache).unwrap();
    let factory = Arc::new(MockFactory::new(|m: &MockBackend, cfg: &UpstreamConfig| {
        let movie = movie_id(&cfg.name);
        let lib = json!({"Items": [{"Id": jmp::ids::hash_id(&["lib", &cfg.name]), "Name": "Movies",
            "Type": "CollectionFolder", "CollectionType": "movies"}], "TotalRecordCount": 1});
        m.on_json(None, "UserViews", lib.clone());
        m.on_json(None, &format!("Users/{}/Views", m.user_id()), lib);
        let legacy = movie.clone();
        m.on_query(None, &format!("Users/{}/Items", m.user_id()), move |q| {
            items_for(&legacy, q)
        });
        m.on_query(None, "Items", move |q| items_for(&movie, q));
        m.on_json(None, "Sessions/Playing", json!({}));
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
    let env = Env {
        state: state.clone(),
        addr,
        base: format!("http://{addr}"),
        http: reqwest::Client::new(),
        _data: data,
    };
    (env, factory)
}

fn movie_id(server: &str) -> String {
    jmp::ids::hash_id(&["movie", server])
}

/// Set by the library test: servers then also hold a second movie.
static SECOND_MOVIE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn items_for(movie: &str, q: &[(String, String)]) -> Value {
    let wanted = q.iter().find(|(k, _)| k == "Ids").map(|(_, v)| v.as_str());
    let hit = wanted.is_none_or(|ids| ids.contains(movie));
    let mut items: Vec<Value> = hit
        .then(|| json!({"Id": movie, "Name": "Big Buck Bunny", "Type": "Movie", "SortName": "big buck bunny"}))
        .into_iter()
        .collect();
    if wanted.is_none() && SECOND_MOVIE.load(std::sync::atomic::Ordering::Relaxed) {
        let id = jmp::ids::hash_id(&["second", movie]);
        items.push(json!({"Id": id, "Name": "Sintel", "Type": "Movie", "SortName": "sintel"}));
    }
    json!({"Items": items, "TotalRecordCount": items.len()})
}

/// The proxy id of a movie, as clients see it.
async fn a_movie(env: &Env, l: &Login, device: &str) -> String {
    let (_, v) = env
        .req(
            &l.token,
            device,
            "GET",
            "Items?Recursive=true&IncludeItemTypes=Movie",
            None,
        )
        .await;
    v["Items"][0]["Id"].as_str().unwrap().to_string()
}

fn hex() -> String {
    hex::encode(rand::random::<[u8; 16]>())
}

/// An unauthenticated socket used to be refused with 401 instead of the 403
/// Jellyfin's `WebSocketManager` gives.
#[tokio::test]
async fn socket_with_a_bad_token_is_forbidden() {
    let (env, _f) = start("").await;
    let url = format!("ws://{}/socket?api_key=nope", env.addr);
    match tokio_tungstenite::connect_async(url).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => {
            assert_eq!(r.status().as_u16(), 403)
        }
        other => panic!("expected an HTTP 403, got {other:?}"),
    }
}

/// The greeting carried no message id and keep-alives were not answered
/// with the same envelope Jellyfin clients parse.
#[tokio::test]
async fn socket_greets_and_answers_keepalive() {
    let (env, _f) = start("").await;
    let me = env.login("me", "pw", "devA").await;
    let url = format!("ws://{}/socket?api_key={}", env.addr, me.token);
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let hello = recv_kind(&mut ws, "ForceKeepAlive").await;
    assert_eq!(hello["Data"], 60);
    assert_eq!(hello["MessageId"].as_str().unwrap().len(), 32, "{hello}");
    send(&mut ws, json!({"MessageType": "KeepAlive"})).await;
    let ka = recv_kind(&mut ws, "KeepAlive").await;
    assert!(ka.get("Data").is_none(), "{ka}");
}

/// A device that reported it can be controlled was not listed for
/// `controllableByUserId`, so no client could find a cast target.
#[tokio::test]
async fn capabilities_make_a_connected_session_controllable() {
    let (env, _f) = start("").await;
    let a = env.login("me", "pw", "devA").await;
    let b = env.login("me", "pw", "devB").await;
    let _ws = env.socket(&a.token).await;
    let q = format!("Sessions?controllableByUserId={}", a.user_id);
    let (_, none) = env.req(&b.token, "devB", "GET", &q, None).await;
    assert_eq!(none, json!([]), "no capabilities yet");
    let (st, _) = env
        .req(
            &a.token,
            "devA",
            "POST",
            "Sessions/Capabilities?supportsMediaControl=true&playableMediaTypes=Audio,Video",
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (_, list) = env.req(&b.token, "devB", "GET", &q, None).await;
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0]["Id"], a.session_id.as_str());
    assert_eq!(list[0]["SupportsRemoteControl"], true);
    assert_eq!(list[0]["PlayableMediaTypes"], json!(["Audio", "Video"]));
    let (_, all) = env.req(&b.token, "devB", "GET", "Sessions", None).await;
    assert_eq!(all.as_array().unwrap().len(), 2);
    let other = hex();
    let (st, _) = env
        .req(
            &b.token,
            "devB",
            "GET",
            &format!("Sessions?controllableByUserId={other}"),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

/// A second login on the same device left two sessions, and the first token
/// kept working; Jellyfin logs the user out of that device first.
#[tokio::test]
async fn logging_in_again_on_a_device_replaces_its_session() {
    let (env, _f) = start("").await;
    let first = env.login("me", "pw", "devA").await;
    let second = env.login("me", "pw", "devA").await;
    let (st, list) = env
        .req(&second.token, "devA", "GET", "Sessions", None)
        .await;
    assert_eq!(st, StatusCode::OK);
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0]["Id"], second.session_id.as_str());
    let (st, _) = env.req(&first.token, "devA", "GET", "Sessions", None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

/// Logging out left the session listed.
#[tokio::test]
async fn logout_removes_the_live_session() {
    let (env, _f) = start("").await;
    let a = env.login("me", "pw", "devA").await;
    let b = env.login("me", "pw", "devB").await;
    let (st, _) = env
        .req(&a.token, "devA", "POST", "Sessions/Logout", None)
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let (_, list) = env.req(&b.token, "devB", "GET", "Sessions", None).await;
    assert_eq!(list.as_array().unwrap().len(), 1, "{list}");
}

#[path = "jellyfin_parity_live/push.rs"]
mod push;
