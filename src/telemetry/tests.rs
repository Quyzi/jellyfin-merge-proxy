use super::*;

#[test]
fn failure_counters_start_at_zero() {
    init_metrics();
    let out = render();
    for line in [
        r#"jmp_config_reloads_total{outcome="error"} "#,
        r#"jmp_tls_reloads_total{target="listener",outcome="error"} "#,
        r#"jmp_tls_reloads_total{target="upstream",outcome="error"} "#,
    ] {
        assert!(out.contains(line), "missing {line}");
    }
}

#[test]
fn templates_are_bounded() {
    assert_eq!(status_class(200), "2xx");
    assert_eq!(status_class(301), "3xx");
    assert_eq!(status_class(404), "4xx");
    assert_eq!(status_class(503), "5xx");
    assert_eq!(
        route_template("/Items/0123456789abcdef0123456789abcdef/Images/Primary"),
        "/items/{id}/images/primary"
    );
    assert_eq!(
        route_template("/videos/0123456789abcdef0123456789abcdef/hls1/main/12.ts"),
        "/videos/{id}/hls1/main/{n}.ts"
    );
    assert_eq!(
        route_template("/Videos/0123456789abcdef0123456789abcdef/stream.mkv"),
        "/videos/{id}/stream.{ext}"
    );
    assert_eq!(
        route_template("/Artists/Some%20Band/Images/Primary"),
        "/artists/{name}/images/primary"
    );
    assert_eq!(route_template("/wp-admin/x.php"), "/other");
    assert_eq!(route_template("/web/assets/a.js"), "/web/*");
    assert_eq!(route_template("/"), "/");
}

#[test]
fn metrics_md_in_sync() {
    let md = metrics_markdown();
    let file = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/METRICS.md"))
        .unwrap_or_default();
    assert_eq!(
        md, file,
        "METRICS.md is stale: run `cargo run -- --metrics-md > METRICS.md`"
    );
    let mut names: Vec<_> = METRICS.iter().map(|m| m.name).collect();
    let n = names.len();
    names.sort();
    names.dedup();
    assert_eq!(n, names.len(), "duplicate metric names");
}
