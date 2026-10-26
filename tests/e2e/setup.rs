//! Test environment setup for e2e tests.

use super::mock::{hex, movie, spawn_mock, Mock};
use serde_json::json;
use std::sync::{Arc, Mutex};

pub struct Pki {
    dir: tempfile::TempDir,
    pub ca_pem: String,
    issuer: rcgen::Issuer<'static, rcgen::KeyPair>,
}

impl Pki {
    pub fn new() -> Self {
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

    pub fn leaf(&self, name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
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

    pub fn ca_path(&self) -> std::path::PathBuf {
        self.dir.path().join("ca.pem")
    }
}

pub fn server_tls(cert: &std::path::Path, key: &std::path::Path) -> Arc<rustls::ServerConfig> {
    let t: jmp::config::TlsConfig = toml::from_str(&format!(
        "cert_path = {:?}\nkey_path = {:?}\n",
        cert.display().to_string(),
        key.display().to_string()
    ))
    .unwrap();
    jmp::tls::server_config(&t).unwrap()
}

pub struct Env {
    pub _data: tempfile::TempDir,
    pub _pki: Pki,
    pub a: Arc<Mock>,
    pub b: Arc<Mock>,
    pub running: jmp::server::Running,
    pub config_path: std::path::PathBuf,
}

pub async fn setup(proxy_tls: bool, extra: &str) -> Env {
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
rewrite_secrets = false

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
