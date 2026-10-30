//! `jmp_upstream_configured{server}` follows the applied `[[server]]` set:
//! 1 from startup, 0 after removal or `enabled = false`, and 1 again when the
//! server comes back, so dashboards and alerts can ignore removed servers.

use jmp::backend::mock::{MockBackend, MockFactory};
use jmp::{AppState, StateOptions};
use std::path::Path;
use std::sync::Arc;

fn config(data_dir: &Path, servers: &str) -> String {
    format!(
        r#"
[proxy]
listen = "127.0.0.1:0"
name = "Configured Gauge"
reload_interval_secs = 0
drain_timeout_secs = 1
data_dir = {data:?}

[[user]]
name = "me"
password = "pw"
{servers}
[logging]
filter = "warn"
"#,
        data = data_dir.display().to_string()
    )
}

fn server(name: &str, enabled: bool) -> String {
    format!(
        "\n[[server]]\nname = \"{name}\"\nserver = \"http://{name}:8096\"\nuser = \"u\"\npassword = \"p\"\nenabled = {enabled}\n"
    )
}

/// Value of `jmp_upstream_configured{server="<name>"}`, if exported.
fn configured(name: &str) -> Option<f64> {
    let prefix = format!("jmp_upstream_configured{{server=\"{name}\"}} ");
    jmp::telemetry::render()
        .lines()
        .find_map(|l| l.strip_prefix(&prefix).map(|v| v.trim().parse().unwrap()))
}

#[tokio::test]
async fn configured_gauge_tracks_removal_disable_and_re_add() {
    jmp::telemetry::init_metrics();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let data = dir.path().join("data");
    let write = |servers: &str| std::fs::write(&path, config(&data, servers)).unwrap();

    let start = format!("{}{}", server("home", true), server("cold", false));
    write(&start);
    let cache = jmp::config::PasswordCache::default();
    let cfg = jmp::config::Config::parse(&config(&data, &start), &cache).unwrap();
    let st = AppState::with_options(
        cfg,
        StateOptions {
            config_path: Some(path.clone()),
            backend_factory: Some(Arc::new(MockFactory::new(|_: &MockBackend, _| {}))),
            password_cache: Some(cache),
            ..Default::default()
        },
    )
    .unwrap();

    // Startup builds the set without a reload: home is configured already;
    // a disabled server is never built and exports nothing.
    assert_eq!(configured("home"), Some(1.0));
    assert_eq!(configured("cold"), None);

    // Add cabin.
    write(&format!(
        "{}{}",
        server("home", true),
        server("cabin", true)
    ));
    assert!(st.reload(false).await.unwrap());
    assert_eq!(
        (configured("home"), configured("cabin")),
        (Some(1.0), Some(1.0))
    );

    // Disable cabin: same as removing it.
    write(&format!(
        "{}{}",
        server("home", true),
        server("cabin", false)
    ));
    assert!(st.reload(false).await.unwrap());
    assert_eq!(st.backends().len(), 1);
    assert_eq!(
        (configured("home"), configured("cabin")),
        (Some(1.0), Some(0.0))
    );

    // Re-enable cabin: back to 1.
    write(&format!(
        "{}{}",
        server("home", true),
        server("cabin", true)
    ));
    assert!(st.reload(false).await.unwrap());
    assert_eq!(configured("cabin"), Some(1.0));

    // Remove the block entirely, then add it back.
    write(&server("home", true));
    assert!(st.reload(false).await.unwrap());
    assert_eq!(
        (configured("home"), configured("cabin")),
        (Some(1.0), Some(0.0))
    );
    write(&format!(
        "{}{}",
        server("home", true),
        server("cabin", true)
    ));
    assert!(st.reload(false).await.unwrap());
    assert_eq!(configured("cabin"), Some(1.0));

    // A rejected reload (bad new server) changes nothing.
    write(&format!(
        "{}{}\n[[server]]\nname = \"bad\"\nserver = \"not a url\"\nuser = \"u\"\npassword = \"p\"\n",
        server("home", true),
        server("cabin", true)
    ));
    assert!(st.reload(false).await.is_err());
    assert_eq!(configured("bad"), None);
    assert_eq!(
        (configured("home"), configured("cabin")),
        (Some(1.0), Some(1.0))
    );
}
