//! E2E test cases.

use super::mock::hex;
use super::setup::{setup, Pki};
use serde_json::{json, Value};

const DATA_LEN: usize = 1000;
const AUTH: &str =
    r#"MediaBrowser Client="Test TV", Device="Living Room", DeviceId="dev1", Version="1.0""#;

fn data() -> Vec<u8> {
    (0..DATA_LEN).map(|i| (i % 251) as u8).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tv_flow_login_views_items_stream_playstate_metrics() {
    let env = setup(false, "").await;
    let base = format!("http://{}", env.running.addr);
    let http = reqwest::Client::new();

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

    let r = http
        .get(format!("{base}/System/Ping"))
        .header("x-request-id", "abc-123")
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["x-request-id"], "abc-123");

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

    let r = http
        .get(format!("{base}/jmp/debug"))
        .header("X-Emby-Token", &token)
        .send()
        .await
        .unwrap();
    assert_ne!(r.status(), 200);

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

    let plain = reqwest::Client::new();
    assert!(plain.get(&url).send().await.is_err());

    let (_c2, k2) = env._pki.leaf("other");
    let text = std::fs::read_to_string(&env.config_path).unwrap();
    let proxy_key = env._pki.dir.path().join("proxy.key");
    std::fs::copy(&k2, &proxy_key).unwrap();
    let err = env.running.state.reload(true).await.unwrap_err();
    assert!(format!("{err:#}").contains("does not match"), "{err:#}");
    let info: Value = http.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(info["ServerName"], "Test Proxy");

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
