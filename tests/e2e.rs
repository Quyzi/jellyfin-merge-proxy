//! End-to-end tests: two mock Jellyfin upstreams (one over TLS with a private
//! CA), the real proxy in between, and a client doing what a TV app does.

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const DATA_LEN: usize = 1000;

fn hex(n: u64) -> String {
    format!("{n:032x}")
}

struct Mock {
    name: &'static str,
    user: String,
    token: String,
    views: Vec<Value>,
    items: Vec<Value>,
    playstate: Mutex<Vec<String>>,
}

fn data() -> Vec<u8> {
    (0..DATA_LEN).map(|i| (i % 251) as u8).collect()
}

async fn mock_handler(State(m): State<Arc<Mock>>, req: Request) -> Response {
    let path = req.uri().path().to_ascii_lowercase();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let headers = req.headers().clone();
    let method = req.method().clone();
    let body = axum::body::to_bytes(req.into_body(), 1 << 20)
        .await
        .unwrap();
    match segs.as_slice() {
        ["system", "info", "public"] => {
            return axum::Json(
                json!({"Version": "10.10.7", "ServerName": m.name, "Id": hex(0xaaaa)}),
            )
            .into_response()
        }
        ["users", "authenticatebyname"] => {
            let v: Value = serde_json::from_slice(&body).unwrap_or_default();
            if v["Username"] == "proxy" && v["Pw"] == "secret" {
                return axum::Json(json!({"AccessToken": m.token, "User": {"Id": m.user}}))
                    .into_response();
            }
            return StatusCode::UNAUTHORIZED.into_response();
        }
        _ => {}
    }
    if headers.get("x-emby-token").and_then(|v| v.to_str().ok()) != Some(m.token.as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match segs.as_slice() {
        ["userviews"] => axum::Json(json!({"Items": m.views, "TotalRecordCount": m.views.len()})).into_response(),
        ["items"] => axum::Json(json!({"Items": m.items, "TotalRecordCount": m.items.len(), "StartIndex": 0})).into_response(),
        ["items", id, "playbackinfo"] => axum::Json(json!({
            "MediaSources": [{
                "Id": id,
                "SupportsDirectPlay": true,
                "TranscodingUrl": format!("/videos/{id}/master.m3u8?MediaSourceId={id}&api_key={}", m.token),
            }],
            "PlaySessionId": "abc"
        }))
        .into_response(),
        ["videos", _id, "stream"] | ["videos", _id, "stream.mkv"] => range_response(&headers),
        ["sessions", "playing", "progress"] if method == axum::http::Method::POST => {
            let v: Value = serde_json::from_slice(&body).unwrap_or_default();
            m.playstate.lock().unwrap().push(v["ItemId"].as_str().unwrap_or("").to_string());
            StatusCode::NO_CONTENT.into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

fn range_response(h: &HeaderMap) -> Response {
    let d = data();
    if let Some(r) = h
        .get("range")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
    {
        let (a, b) = r.split_once('-').unwrap();
        let a: usize = a.parse().unwrap();
        let b: usize = if b.is_empty() {
            DATA_LEN - 1
        } else {
            b.parse().unwrap()
        };
        return Response::builder()
            .status(206)
            .header("content-type", "video/x-matroska")
            .header("accept-ranges", "bytes")
            .header("content-range", format!("bytes {a}-{b}/{DATA_LEN}"))
            .header("content-length", (b - a + 1).to_string())
            .body(Body::from(d[a..=b].to_vec()))
            .unwrap();
    }
    Response::builder()
        .header("content-type", "video/x-matroska")
        .header("accept-ranges", "bytes")
        .body(Body::from(d))
        .unwrap()
}

async fn spawn_mock(m: Arc<Mock>, tls: Option<Arc<rustls::ServerConfig>>) -> u16 {
    let app = axum::Router::new().fallback(mock_handler).with_state(m);
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.set_nonblocking(true).unwrap();
    let port = l.local_addr().unwrap().port();
    match tls {
        Some(c) => {
            let rc = axum_server::tls_rustls::RustlsConfig::from_config(c);
            tokio::spawn(
                axum_server::tls_rustls::from_tcp_rustls(l, rc)
                    .unwrap()
                    .serve(app.into_make_service()),
            );
        }
        None => {
            tokio::spawn(
                axum_server::from_tcp(l)
                    .unwrap()
                    .serve(app.into_make_service()),
            );
        }
    }
    port
}

struct Pki {
    dir: tempfile::TempDir,
    ca_pem: String,
    issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
}

impl Pki {
    fn new() -> Self {
        let mut p = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        p.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        p.distinguished_name
            .push(rcgen::DnType::CommonName, "jmp test CA");
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = p.self_signed(&key).unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ca.pem"), cert.pem()).unwrap();
        let issuer = rcgen::Issuer::new(p, key);
        Pki {
            dir,
            ca_pem: cert.pem(),
            issuer,
        }
    }

    /// Issue a leaf for `localhost`, write `<name>.pem` / `<name>.key`.
    fn leaf(&self, name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let p =
            rcgen::CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])
                .unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = p.signed_by(&key, &self.issuer).unwrap();
        let c = self.dir.path().join(format!("{name}.pem"));
        let k = self.dir.path().join(format!("{name}.key"));
        std::fs::write(&c, cert.pem()).unwrap();
        std::fs::write(&k, key.serialize_pem()).unwrap();
        (c, k)
    }

    fn ca_path(&self) -> std::path::PathBuf {
        self.dir.path().join("ca.pem")
    }
}

fn server_tls(cert: &std::path::Path, key: &std::path::Path) -> Arc<rustls::ServerConfig> {
    let t: jmp::config::TlsConfig = toml::from_str(&format!(
        "cert_path = {:?}\nkey_path = {:?}\n",
        cert.display().to_string(),
        key.display().to_string()
    ))
    .unwrap();
    jmp::tls::server_config(&t).unwrap()
}

fn movie(id: u64, name: &str) -> Value {
    json!({"Id": hex(id), "ServerId": hex(0xaaaa), "Name": name, "SortName": name.to_lowercase(), "Type": "Movie", "MediaType": "Video", "ImageTags": {"Primary": "x"}})
}

struct Env {
    _data: tempfile::TempDir,
    _pki: Pki,
    a: Arc<Mock>,
    b: Arc<Mock>,
    running: jmp::server::Running,
    config_path: std::path::PathBuf,
}

async fn setup(proxy_tls: bool, extra: &str) -> Env {
    jmp::tls::install_default_provider();
    let pki = Pki::new();
    let a = Arc::new(Mock {
        name: "A",
        user: hex(0xa0),
        token: "tokenA".into(),
        views: vec![
            json!({"Id": hex(0xa1), "Name": "Movies", "CollectionType": "movies", "Type": "CollectionFolder"}),
        ],
        items: vec![movie(0xa2, "Alpha"), movie(0xa3, "Charlie")],
        playstate: Mutex::new(vec![]),
    });
    let b = Arc::new(Mock {
        name: "B",
        user: hex(0xb0),
        token: "tokenB".into(),
        views: vec![
            json!({"Id": hex(0xb1), "Name": "Movies", "CollectionType": "movies", "Type": "CollectionFolder"}),
            json!({"Id": hex(0xb4), "Name": "Shows", "CollectionType": "tvshows", "Type": "CollectionFolder"}),
        ],
        items: vec![movie(0xb2, "Bravo")],
        playstate: Mutex::new(vec![]),
    });
    let port_a = spawn_mock(a.clone(), None).await;
    let (bc, bk) = pki.leaf("upstream-b");
    let port_b = spawn_mock(b.clone(), Some(server_tls(&bc, &bk))).await;

    let data = tempfile::tempdir().unwrap();
    let mut text = format!(
        r#"
[proxy]
listen = "127.0.0.1:0"
name = "Test Proxy"
data_dir = {data:?}
reload_interval_secs = 0

[[user]]
name = "family"
password = "pw"

[[server]]
name = "a"
server = "http://127.0.0.1:{port_a}"
user = "proxy"
password = "secret"

[[server]]
name = "b"
server = "https://localhost:{port_b}"
user = "proxy"
password = "secret"
[server.tls]
ca_path = {ca:?}

[logging]
filter = "warn"
"#,
        data = data.path().display().to_string(),
        ca = pki.ca_path().display().to_string(),
    );
    if proxy_tls {
        let (c, k) = pki.leaf("proxy");
        text.push_str(&format!(
            "\n[tls]\ncert_path = {:?}\nkey_path = {:?}\nmin_version = \"1.2\"\n",
            c.display().to_string(),
            k.display().to_string()
        ));
    }
    text.push_str(extra);
    let config_path = data.path().join("config.toml");
    std::fs::write(&config_path, &text).unwrap();
    let cache = jmp::config::PasswordCache::default();
    let cfg = jmp::config::Config::load(&config_path, &cache).unwrap();
    let running = jmp::server::start(
        cfg,
        jmp::server::Options {
            config_path: Some(config_path.clone()),
            password_cache: Some(cache),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    Env {
        _data: data,
        _pki: pki,
        a,
        b,
        running,
        config_path,
    }
}

const AUTH: &str =
    r#"MediaBrowser Client="Test TV", Device="Living Room", DeviceId="dev1", Version="1.0""#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tv_flow_login_views_items_stream_playstate_metrics() {
    let env = setup(false, "").await;
    let base = format!("http://{}", env.running.addr);
    let http = reqwest::Client::new();

    // Public endpoints a TV app calls before login.
    let info: Value = http
        .get(format!("{base}/System/Info/Public"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["ServerName"], "Test Proxy");
    let server_id = info["Id"].as_str().unwrap().to_string();
    assert_eq!(
        http.get(format!("{base}/System/Ping"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        http.get(format!("{base}/Branding/Configuration"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let qc: Value = http
        .get(format!("{base}/QuickConnect/Enabled"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(qc, json!(false));
    let pubu: Value = http
        .get(format!("{base}/Users/Public"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(pubu, json!([]));

    // Wrong password, then no token.
    let bad = http
        .post(format!("{base}/Users/AuthenticateByName"))
        .header("Authorization", AUTH)
        .json(&json!({"Username": "family", "Pw": "nope"}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401);
    assert_eq!(
        http.get(format!("{base}/UserViews"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );

    // Login.
    let auth: Value = http
        .post(format!("{base}/Users/AuthenticateByName"))
        .header("Authorization", AUTH)
        .json(&json!({"Username": "family", "Pw": "pw"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = auth["AccessToken"].as_str().unwrap().to_string();
    let uid = auth["User"]["Id"].as_str().unwrap().to_string();
    assert_eq!(auth["ServerId"], server_id.as_str());
    assert!(auth["SessionInfo"]["Id"].is_string());
    assert_ne!(token, "tokenA");

    // Merged views (legacy route, Authorization header with Token=).
    let views: Value = http
        .get(format!("{base}/Users/{uid}/Views"))
        .header("Authorization", format!("{AUTH}, Token=\"{token}\""))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: Vec<&str> = views["Items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["Name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["Movies", "Shows"],
        "Movies merged across both upstreams"
    );
    let movies_id = views["Items"][0]["Id"].as_str().unwrap().to_string();

    // Merged + sorted items (X-Emby-Token).
    let items: Value = http
        .get(format!("{base}/Items?ParentId={movies_id}&SortBy=SortName&SortOrder=Ascending&StartIndex=0&Limit=10&userId={uid}"))
        .header("X-Emby-Token", &token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: Vec<&str> = items["Items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["Name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Alpha", "Bravo", "Charlie"]);
    assert_eq!(items["TotalRecordCount"], 3);
    for it in items["Items"].as_array().unwrap() {
        let id = it["Id"].as_str().unwrap();
        assert!(
            ![hex(0xa2), hex(0xa3), hex(0xb2)].contains(&id.to_string()),
            "upstream ids must be rewritten"
        );
        assert_eq!(it["ServerId"], server_id.as_str());
    }
    let bravo = items["Items"][1]["Id"].as_str().unwrap().to_string();

    // PlaybackInfo: URLs point back to the proxy with the client token.
    let pi: Value = http
        .post(format!("{base}/Items/{bravo}/PlaybackInfo?UserId={uid}"))
        .header("X-Emby-Authorization", format!("{AUTH}, Token=\"{token}\""))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let tu = pi["MediaSources"][0]["TranscodingUrl"].as_str().unwrap();
    assert!(
        tu.starts_with(&format!("/videos/{bravo}/master.m3u8")),
        "{tu}"
    );
    assert!(
        tu.contains(&format!("api_key={token}")) && !tu.contains("tokenB"),
        "{tu}"
    );

    // Range stream from the TLS upstream, token via api_key query param.
    let r = http
        .get(format!(
            "{base}/Videos/{bravo}/stream?static=true&api_key={token}"
        ))
        .header("Range", "bytes=10-19")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 206);
    assert_eq!(r.headers()["content-range"], "bytes 10-19/1000");
    let body = r.bytes().await.unwrap();
    assert_eq!(&body[..], &data()[10..20]);
    // Full stream.
    let full = http
        .get(format!("{base}/Videos/{bravo}/stream?static=true"))
        .header("X-MediaBrowser-Token", &token)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(full.len(), DATA_LEN);

    // Playstate goes to the owning upstream, with the upstream id.
    let r = http
        .post(format!("{base}/Sessions/Playing/Progress"))
        .header("X-Emby-Token", &token)
        .json(&json!({"ItemId": bravo, "PositionTicks": 1000}))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    assert_eq!(env.b.playstate.lock().unwrap().clone(), vec![hex(0xb2)]);
    assert!(env.a.playstate.lock().unwrap().is_empty());

    // Request ids are propagated.
    let r = http
        .get(format!("{base}/System/Ping"))
        .header("x-request-id", "abc-123")
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["x-request-id"], "abc-123");

    // Metrics.
    let m = http
        .get(format!("{base}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    for name in [
        "jmp_http_requests_total",
        "jmp_http_request_duration_seconds",
        "jmp_stage_duration_seconds",
        "jmp_upstream_requests_total",
        "jmp_upstream_auth_total",
        "jmp_fanout_upstream_results_total",
        "jmp_merge_items_in_total",
        "jmp_ids_decoded_total",
        "jmp_logins_total",
        "jmp_token_validations_total",
        "jmp_argon2_duration_seconds",
        "jmp_stream_bytes_total",
        "jmp_stream_end_total",
        "jmp_range_requests_total",
        "jmp_cache_requests_total",
        "jmp_sessions_active",
        "jmp_build_info",
        "jmp_uptime_seconds",
    ] {
        assert!(m.contains(name), "missing metric {name}");
    }
    assert!(
        m.contains(r#"route="/videos/{id}/stream""#),
        "route templates are used as labels"
    );
    assert!(
        !m.contains(&token) && !m.contains("tokenA") && !m.contains(&bravo),
        "no secrets or ids in metrics"
    );

    // Debug endpoint is off by default.
    let r = http
        .get(format!("{base}/jmp/debug"))
        .header("X-Emby-Token", &token)
        .send()
        .await
        .unwrap();
    assert_ne!(r.status(), 200);

    // Reload with a broken config keeps the old one.
    let gen = env
        .running
        .state
        .generation
        .load(std::sync::atomic::Ordering::Relaxed);
    let good = std::fs::read_to_string(&env.config_path).unwrap();
    std::fs::write(&env.config_path, "this is = = not toml").unwrap();
    assert!(env.running.state.reload(false).await.is_err());
    assert_eq!(
        env.running
            .state
            .generation
            .load(std::sync::atomic::Ordering::Relaxed),
        gen
    );
    // A valid change (new user) applies.
    std::fs::write(
        &env.config_path,
        good.replace(
            "password = \"pw\"\n",
            "password = \"pw\"\n\n[[user]]\nname = \"guest\"\npassword = \"g\"\n",
        ),
    )
    .unwrap();
    assert!(env.running.state.reload(false).await.unwrap());
    assert_eq!(env.running.state.authenticator.list().len(), 2);
    env.running.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_listener_and_cert_validation_on_reload() {
    let env = setup(true, "").await;
    assert!(env.running.tls);
    let ca = reqwest::Certificate::from_pem(env._pki.ca_pem.as_bytes()).unwrap();
    let http = reqwest::Client::builder()
        .add_root_certificate(ca)
        .build()
        .unwrap();
    let url = format!(
        "https://localhost:{}/System/Info/Public",
        env.running.addr.port()
    );
    let info: Value = http.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(info["ServerName"], "Test Proxy");

    // Untrusting client fails.
    let plain = reqwest::Client::new();
    assert!(plain.get(&url).send().await.is_err());

    // Replace the key with one that doesn't match: reload must fail and the
    // listener keeps serving the old certificate.
    let (_c2, k2) = env._pki.leaf("other");
    let text = std::fs::read_to_string(&env.config_path).unwrap();
    let proxy_key = env._pki.dir.path().join("proxy.key");
    std::fs::copy(&k2, &proxy_key).unwrap();
    let err = env.running.state.reload(true).await.unwrap_err();
    assert!(format!("{err:#}").contains("does not match"), "{err:#}");
    let info: Value = http.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(info["ServerName"], "Test Proxy");

    // A fresh matching pair hot-reloads.
    let (c3, k3) = env._pki.leaf("proxy3");
    let text = text
        .replace(
            &env._pki.dir.path().join("proxy.pem").display().to_string(),
            &c3.display().to_string(),
        )
        .replace(&proxy_key.display().to_string(), &k3.display().to_string());
    std::fs::write(&env.config_path, text).unwrap();
    assert!(env.running.state.reload(true).await.unwrap());
    let fresh = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(env._pki.ca_pem.as_bytes()).unwrap())
        .build()
        .unwrap();
    assert_eq!(fresh.get(&url).send().await.unwrap().status(), 200);
    env.running.shutdown();
}

#[test]
fn upstream_tls_requires_matching_ca() {
    jmp::tls::install_default_provider();
    let pki = Pki::new();
    let t: jmp::config::UpstreamTlsConfig = toml::from_str(&format!(
        "ca_path = {:?}",
        pki.ca_path().display().to_string()
    ))
    .unwrap();
    assert!(jmp::tls::client_config(&t, "x").unwrap().is_some());
    let bad: jmp::config::UpstreamTlsConfig =
        toml::from_str("ca_path = \"/nonexistent/ca.pem\"").unwrap();
    assert!(jmp::tls::client_config(&bad, "x").is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn debug_endpoint_and_separate_metrics_port() {
    let env = setup(
        false,
        "\n[metrics]\nlisten = \"127.0.0.1:0\"\n\n[debug]\nenabled = true\nusers = [\"family\"]\n",
    )
    .await;
    let base = format!("http://{}", env.running.addr);
    let http = reqwest::Client::new();
    assert_eq!(
        http.get(format!("{base}/jmp/debug"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let auth: Value = http
        .post(format!("{base}/Users/AuthenticateByName"))
        .header("Authorization", AUTH)
        .json(&json!({"Username": "family", "Pw": "pw"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = auth["AccessToken"].as_str().unwrap().to_string();
    // Trigger upstream logins + an id mapping.
    let _ = http
        .get(format!("{base}/UserViews"))
        .header("X-Emby-Token", &token)
        .send()
        .await
        .unwrap();
    let r = http
        .get(format!("{base}/jmp/debug"))
        .header("X-Emby-Token", &token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let text = r.text().await.unwrap();
    assert!(
        !text.contains(&token) && !text.contains("tokenA") && !text.contains("secret"),
        "debug dump leaks a secret"
    );
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["upstreams"].as_array().unwrap().len(), 2);
    assert_eq!(v["sessions"].as_array().unwrap().len(), 1);
    assert!(v["config"]["generation"].as_u64().unwrap() >= 1);

    // Metrics live only on the separate port.
    let maddr = env.running.metrics_addr.expect("metrics listener");
    let m = http
        .get(format!("http://{maddr}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(m.contains("jmp_upstream_up"));
    assert_ne!(
        http.get(format!("{base}/metrics"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    env.running.shutdown();
}
