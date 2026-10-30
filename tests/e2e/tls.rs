//! TLS: the proxy's own listener and upstream certificate validation.

use super::*;

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
