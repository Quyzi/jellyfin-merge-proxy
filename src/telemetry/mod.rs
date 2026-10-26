//! Logging and metrics.
//!
//! All metric names live in [`names`] and are described in [`metrics::METRICS`]
//! (which also generates METRICS.md via `jellyfin-merge-proxy --metrics-md`).
//! Label values are always bounded: route templates, upstream names, and
//! small enums - never raw paths, item ids, or tokens.

use ::metrics::gauge;
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub mod logging;
pub mod metrics;
pub mod names;
pub mod spans;
pub mod streams;

pub use logging::{init_logging, LogHandle};
pub use metrics::metrics_markdown;
pub use metrics::{Kind, MetricDef, METRICS};
pub use spans::{
    argon2_duration, http_error, http_layer, make_span, route_template, stream_kind, RequestId,
    Stage,
};
pub use streams::{active_streams, MeteredStream, StreamInfo};

static PROM: OnceLock<PrometheusHandle> = OnceLock::new();
static PROCESS: OnceLock<metrics_process::Collector> = OnceLock::new();
static STARTED: OnceLock<Instant> = OnceLock::new();

pub fn git_sha() -> &'static str {
    option_env!("JMP_GIT_SHA").unwrap_or("unknown")
}

pub fn init_metrics() -> PrometheusHandle {
    STARTED.get_or_init(Instant::now);
    PROM.get_or_init(|| {
        let secs = [
            0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
        ];
        let bytes = [
            256.0, 1024.0, 4096.0, 16384.0, 65536.0, 262144.0, 1048576.0, 4194304.0,
        ];
        let counts = [1.0, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0, 16.0];
        let handle = PrometheusBuilder::new()
            .set_buckets_for_metric(Matcher::Suffix("_seconds".into()), &secs)
            .and_then(|b| b.set_buckets_for_metric(Matcher::Suffix("_bytes".into()), &bytes))
            .and_then(|b| {
                b.set_buckets_for_metric(Matcher::Full(names::FANOUT_UPSTREAMS.into()), &counts)
            })
            .expect("buckets")
            .install_recorder()
            .expect("install prometheus recorder");
        for m in METRICS {
            match m.kind {
                Kind::Counter => ::metrics::describe_counter!(m.name, m.help),
                Kind::Gauge => ::metrics::describe_gauge!(m.name, m.help),
                Kind::Histogram => ::metrics::describe_histogram!(m.name, m.help),
            }
        }
        gauge!(names::BUILD_INFO, "version" => env!("CARGO_PKG_VERSION"), "git_sha" => git_sha())
            .set(1.0);
        handle
    })
    .clone()
}

pub fn enable_process_metrics() {
    PROCESS.get_or_init(|| {
        let c = metrics_process::Collector::default();
        c.describe();
        c
    });
}

pub fn collect_on_scrape() {
    if let Some(c) = PROCESS.get() {
        c.collect();
    }
    if let Some(s) = STARTED.get() {
        gauge!(names::UPTIME).set(s.elapsed().as_secs_f64());
    }
    if let Ok(h) = tokio::runtime::Handle::try_current() {
        let m = h.metrics();
        gauge!(names::TOKIO_WORKERS).set(m.num_workers() as f64);
        gauge!(names::TOKIO_ALIVE_TASKS).set(m.num_alive_tasks() as f64);
        gauge!(names::TOKIO_GLOBAL_QUEUE).set(m.global_queue_depth() as f64);
    }
}

pub fn render() -> String {
    collect_on_scrape();
    match PROM.get() {
        Some(h) => {
            h.run_upkeep();
            h.render()
        }
        None => String::new(),
    }
}

pub fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_are_bounded() {
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
}
