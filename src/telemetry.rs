//! Logging and metrics.
//!
//! All metric names live in [`names`] and are described in [`METRICS`]
//! (which also generates METRICS.md via `jellyfin-merge-proxy --metrics-md`).
//! Label values are always bounded: route templates, upstream names, and
//! small enums - never raw paths, item ids, or tokens.

use axum::extract::Request;
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;
use bytes::Bytes;
use futures_util::Stream;
use metrics::{counter, gauge, histogram};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub mod names {
    pub const HTTP_REQUESTS: &str = "jmp_http_requests_total";
    pub const HTTP_DURATION: &str = "jmp_http_request_duration_seconds";
    pub const HTTP_IN_FLIGHT: &str = "jmp_http_requests_in_flight";
    pub const HTTP_RESPONSE_SIZE: &str = "jmp_http_response_size_bytes";
    pub const HTTP_ERRORS: &str = "jmp_http_errors_total";
    pub const STAGE_DURATION: &str = "jmp_stage_duration_seconds";

    pub const FANOUT_UPSTREAMS: &str = "jmp_fanout_upstreams";
    pub const FANOUT_RESULTS: &str = "jmp_fanout_upstream_results_total";
    pub const FANOUT_PARTIAL: &str = "jmp_fanout_partial_total";
    pub const FANOUT_SLOWEST: &str = "jmp_fanout_slowest_seconds";

    pub const IDS_ENCODED: &str = "jmp_ids_encoded_total";
    pub const IDS_NEW: &str = "jmp_ids_new_total";
    pub const IDS_DECODED: &str = "jmp_ids_decoded_total";
    pub const IDS_TABLE_SIZE: &str = "jmp_ids_table_size";

    pub const MERGE_ITEMS_IN: &str = "jmp_merge_items_in_total";
    pub const MERGE_ITEMS_OUT: &str = "jmp_merge_items_out_total";
    pub const DEDUPE_DROPPED: &str = "jmp_dedupe_dropped_total";
    pub const DEDUPE_WINNER: &str = "jmp_dedupe_winner_total";
    pub const LIBRARY_GROUPS: &str = "jmp_library_groups";

    pub const CACHE_REQUESTS: &str = "jmp_cache_requests_total";
    pub const CACHE_ENTRIES: &str = "jmp_cache_entries";
    pub const CACHE_BYTES: &str = "jmp_cache_bytes";
    pub const CACHE_REFRESH_DURATION: &str = "jmp_cache_refresh_duration_seconds";
    pub const CACHE_REFRESH_FAILURES: &str = "jmp_cache_refresh_failures_total";
    pub const CACHE_LAST_REFRESH: &str = "jmp_cache_last_refresh_timestamp_seconds";
    pub const CACHE_EVICTIONS: &str = "jmp_cache_evictions_total";
    pub const PERSIST_OPS: &str = "jmp_persist_ops_total";

    pub const UPSTREAM_REQUESTS: &str = "jmp_upstream_requests_total";
    pub const UPSTREAM_DURATION: &str = "jmp_upstream_request_duration_seconds";
    pub const UPSTREAM_ERRORS: &str = "jmp_upstream_errors_total";
    pub const UPSTREAM_IN_FLIGHT: &str = "jmp_upstream_in_flight";
    pub const UPSTREAM_UP: &str = "jmp_upstream_up";
    pub const UPSTREAM_RETRIES: &str = "jmp_upstream_retries_total";
    pub const UPSTREAM_REAUTH: &str = "jmp_upstream_reauth_total";
    pub const UPSTREAM_AUTH: &str = "jmp_upstream_auth_total";
    pub const UPSTREAM_TOKEN_AGE: &str = "jmp_upstream_token_age_seconds";

    pub const SESSIONS_ACTIVE: &str = "jmp_sessions_active";
    pub const LOGINS: &str = "jmp_logins_total";
    pub const TOKEN_VALIDATIONS: &str = "jmp_token_validations_total";
    pub const ARGON2_DURATION: &str = "jmp_argon2_duration_seconds";

    pub const STREAMS_ACTIVE: &str = "jmp_streams_active";
    pub const STREAM_BYTES: &str = "jmp_stream_bytes_total";
    pub const STREAM_TTFB: &str = "jmp_stream_ttfb_seconds";
    pub const STREAM_END: &str = "jmp_stream_end_total";
    pub const RANGE_REQUESTS: &str = "jmp_range_requests_total";
    pub const HLS_PLAYLIST_REWRITES: &str = "jmp_hls_playlist_rewrites_total";
    pub const REWRITES: &str = "jmp_rewrites_total";
    pub const HLS_SEGMENT_REQUESTS: &str = "jmp_hls_segment_requests_total";

    pub const CONFIG_RELOADS: &str = "jmp_config_reloads_total";
    pub const CONFIG_LAST_RELOAD: &str = "jmp_config_last_reload_timestamp_seconds";
    pub const CONFIG_GENERATION: &str = "jmp_config_generation";
    pub const CONFIG_UPSTREAMS: &str = "jmp_config_upstreams";
    pub const SERVERS_ACTIVE: &str = "jmp_servers_active";
    pub const SERVERS_DRAINING: &str = "jmp_servers_draining";
    pub const SERVERS_RETIRED: &str = "jmp_servers_retired_total";
    pub const SERVER_DRAIN_DURATION: &str = "jmp_server_drain_duration_seconds";
    pub const REMOVED_SERVER_REQUESTS: &str = "jmp_removed_server_requests_total";
    pub const CONFIG_USERS: &str = "jmp_config_users";
    pub const CONFIG_UPSTREAM_CHANGES: &str = "jmp_config_upstream_changes_total";
    pub const TLS_RELOADS: &str = "jmp_tls_reloads_total";
    pub const TLS_CERT_EXPIRY: &str = "jmp_tls_cert_expiry_timestamp_seconds";

    pub const BUILD_INFO: &str = "jmp_build_info";
    pub const UPTIME: &str = "jmp_uptime_seconds";
    pub const TOKIO_WORKERS: &str = "jmp_tokio_workers";
    pub const TOKIO_ALIVE_TASKS: &str = "jmp_tokio_alive_tasks";
    pub const TOKIO_GLOBAL_QUEUE: &str = "jmp_tokio_global_queue_depth";
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Counter,
    Gauge,
    Histogram,
}

pub struct MetricDef {
    pub name: &'static str,
    pub kind: Kind,
    pub labels: &'static str,
    pub help: &'static str,
}

macro_rules! m {
    ($n:expr, $k:ident, $l:expr, $h:expr) => {
        MetricDef {
            name: $n,
            kind: Kind::$k,
            labels: $l,
            help: $h,
        }
    };
}

use names::*;
/// Every metric the proxy emits (process_* metrics come from metrics-process).
pub const METRICS: &[MetricDef] = &[
    m!(HTTP_REQUESTS, Counter, "route, method, status", "Client HTTP requests handled, by route template, method and status code."),
    m!(HTTP_DURATION, Histogram, "route, method", "Time until response headers were produced (streams: excludes body transfer)."),
    m!(HTTP_IN_FLIGHT, Gauge, "", "Client requests currently being processed (headers not yet sent)."),
    m!(HTTP_RESPONSE_SIZE, Histogram, "route", "Response body size when known up-front (Content-Length)."),
    m!(HTTP_ERRORS, Counter, "kind", "Requests that failed inside the proxy, by error kind (unauthorized, not_found, upstream_unavailable, upstream_error, bad_request, payload_too_large)."),
    m!(STAGE_DURATION, Histogram, "stage", "Duration of internal pipeline stages: auth, id_decode, fanout, rewrite, merge, dedupe, sort_page, serialize, views_build."),
    m!(FANOUT_UPSTREAMS, Histogram, "endpoint", "Number of upstreams queried by one fan-out request."),
    m!(FANOUT_RESULTS, Counter, "endpoint, server, outcome", "Per-upstream result of fan-out calls: success, http_error, timeout, error, skipped (endpoint=views: library list refresh)."),
    m!(FANOUT_PARTIAL, Counter, "endpoint", "Fan-out responses served with results from only some of the targeted upstreams (endpoint=views: a server's libraries were missing from the list)."),
    m!(FANOUT_SLOWEST, Histogram, "endpoint", "Time spent waiting for the slowest upstream of a fan-out."),
    m!(IDS_ENCODED, Counter, "server", "Upstream ids rewritten to proxy ids."),
    m!(IDS_NEW, Counter, "server", "New entries added to the id mapping table."),
    m!(IDS_DECODED, Counter, "outcome", "Proxy id lookups: hit, view, miss, invalid."),
    m!(IDS_TABLE_SIZE, Gauge, "server", "Entries in the id mapping table."),
    m!(MERGE_ITEMS_IN, Counter, "endpoint", "Items received from upstreams for merged lists."),
    m!(MERGE_ITEMS_OUT, Counter, "endpoint", "Items returned to clients from merged lists (after dedupe and paging)."),
    m!(DEDUPE_DROPPED, Counter, "key", "Duplicates removed, by match key: tmdb, imdb, tvdb, title (search hints: type+name+year), index (season/episode number of a show split across servers), name."),
    m!(DEDUPE_WINNER, Counter, "server", "Dedupe decisions won by each upstream."),
    m!(LIBRARY_GROUPS, Gauge, "kind", "Library views presented: merged (combined across servers) or single."),
    m!(CACHE_REQUESTS, Counter, "cache, outcome", "Catalog cache lookups: hit, miss, stale (refresh forced/expired)."),
    m!(CACHE_ENTRIES, Gauge, "cache, server", "Entries held in the catalog cache."),
    m!(CACHE_BYTES, Gauge, "cache, server", "Approximate serialized size of the catalog cache."),
    m!(CACHE_REFRESH_DURATION, Histogram, "cache, server", "Duration of catalog refresh calls to an upstream."),
    m!(CACHE_REFRESH_FAILURES, Counter, "cache, server", "Failed catalog refreshes."),
    m!(CACHE_LAST_REFRESH, Gauge, "cache, server", "Unix time of the last successful catalog refresh."),
    m!(CACHE_EVICTIONS, Counter, "cache", "Catalog cache entries replaced/evicted."),
    m!(PERSIST_OPS, Counter, "store, op, outcome", "Persistence operations (ids/sessions load and save)."),
    m!(UPSTREAM_REQUESTS, Counter, "server, endpoint, status_class", "HTTP requests sent to upstreams by route template and status class (2xx..5xx, error)."),
    m!(UPSTREAM_DURATION, Histogram, "server, endpoint", "Upstream time to response headers."),
    m!(UPSTREAM_ERRORS, Counter, "server, kind", "Upstream transport errors: connect, dns, tls, timeout, body, other."),
    m!(UPSTREAM_IN_FLIGHT, Gauge, "server", "Concurrent requests to each upstream (headers pending)."),
    m!(UPSTREAM_UP, Gauge, "server", "1 if the upstream is considered healthy, 0 while in back-off."),
    m!(UPSTREAM_RETRIES, Counter, "server, reason", "Upstream request retries (reason: unauthorized)."),
    m!(UPSTREAM_REAUTH, Counter, "server", "Re-authentications triggered by a 401."),
    m!(UPSTREAM_AUTH, Counter, "server, outcome", "Upstream logins: success, failure, unreachable."),
    m!(UPSTREAM_TOKEN_AGE, Gauge, "server", "Seconds since the upstream token was obtained."),
    m!(SESSIONS_ACTIVE, Gauge, "", "Proxy client sessions (issued tokens)."),
    m!(LOGINS, Counter, "outcome", "Proxy logins: success, unknown_user, bad_password, bad_request."),
    m!(TOKEN_VALIDATIONS, Counter, "outcome", "Client token checks: valid, invalid, missing, anonymous."),
    m!(ARGON2_DURATION, Histogram, "op", "argon2 hash/verify time."),
    m!(STREAMS_ACTIVE, Gauge, "server, kind", "Response bodies currently being streamed (kind: direct, hls_segment, hls_playlist, transcode, image, download, web, other)."),
    m!(STREAM_BYTES, Counter, "server, kind", "Bytes streamed from upstreams to clients."),
    m!(STREAM_TTFB, Histogram, "server, kind", "Time from upstream request to first body byte."),
    m!(STREAM_END, Counter, "server, kind, outcome", "Finished streams: complete, client_disconnect, upstream_error."),
    m!(RANGE_REQUESTS, Counter, "server", "Requests forwarded with a Range header."),
    m!(HLS_PLAYLIST_REWRITES, Counter, "server", "HLS playlists rewritten to point at the proxy."),
    m!(REWRITES, Counter, "rewriter", "Response rewriter stages applied (json_ids, hls, location, ...)."),
    m!(HLS_SEGMENT_REQUESTS, Counter, "server", "HLS segment requests proxied."),
    m!(CONFIG_RELOADS, Counter, "outcome", "Config reload attempts: success, error, unchanged."),
    m!(CONFIG_LAST_RELOAD, Gauge, "", "Unix time of the last successful config (re)load."),
    m!(CONFIG_GENERATION, Gauge, "", "Incremented on each successfully applied config."),
    m!(CONFIG_UPSTREAMS, Gauge, "", "Configured enabled upstreams."),
    m!(CONFIG_USERS, Gauge, "", "Configured proxy users."),
    m!(CONFIG_UPSTREAM_CHANGES, Counter, "change", "[[server]] changes applied on reload: added, removed, rebuilt (server URL changed), changed (TLS/credentials/timeout, in place)."),
    m!(SERVERS_ACTIVE, Gauge, "", "Backends currently serving requests (enabled [[server]] entries)."),
    m!(SERVERS_DRAINING, Gauge, "", "Removed or rebuilt backends still finishing in-flight requests/streams."),
    m!(SERVERS_RETIRED, Counter, "outcome", "Backends retired after removal/rebuild: drained (all requests finished) or timeout (proxy.drain_timeout_secs hit)."),
    m!(SERVER_DRAIN_DURATION, Histogram, "", "Time from removal/rebuild until a backend was retired."),
    m!(REMOVED_SERVER_REQUESTS, Counter, "", "Requests for ids of a [[server]] that was removed (answered 410 Gone)."),
    m!(TLS_RELOADS, Counter, "target, outcome", "TLS material reloads (target: listener, upstream)."),
    m!(TLS_CERT_EXPIRY, Gauge, "cert", "notAfter of loaded certificates as unix time (cert: listener or upstream:<name>)."),
    m!(BUILD_INFO, Gauge, "version, git_sha", "Always 1; build metadata in labels."),
    m!(UPTIME, Gauge, "", "Seconds since process start."),
    m!(TOKIO_WORKERS, Gauge, "", "Tokio runtime worker threads."),
    m!(TOKIO_ALIVE_TASKS, Gauge, "", "Tokio tasks currently alive."),
    m!(TOKIO_GLOBAL_QUEUE, Gauge, "", "Tasks waiting in the Tokio global queue."),
];

pub fn metrics_markdown() -> String {
    let mut s = String::from("# Metrics\n\nGenerated by `jellyfin-merge-proxy --metrics-md`. All metrics are Prometheus-formatted at the configured `[metrics]` endpoint. Process metrics (`process_cpu_seconds_total`, `process_resident_memory_bytes`, `process_open_fds`, ...) are added by the `metrics-process` crate when `process_metrics = true`.\n\n| Name | Type | Labels | Meaning |\n|---|---|---|---|\n");
    for m in METRICS {
        let k = match m.kind {
            Kind::Counter => "counter",
            Kind::Gauge => "gauge",
            Kind::Histogram => "histogram",
        };
        s.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            m.name,
            k,
            if m.labels.is_empty() { "-" } else { m.labels },
            m.help
        ));
    }
    s
}

// ---------------------------------------------------------------------------
// Recorder
// ---------------------------------------------------------------------------

static PROM: OnceLock<PrometheusHandle> = OnceLock::new();
static PROCESS: OnceLock<metrics_process::Collector> = OnceLock::new();
static STARTED: OnceLock<Instant> = OnceLock::new();

pub fn git_sha() -> &'static str {
    option_env!("JMP_GIT_SHA").unwrap_or("unknown")
}

/// Install the global Prometheus recorder (idempotent).
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
            .and_then(|b| b.set_buckets_for_metric(Matcher::Full(FANOUT_UPSTREAMS.into()), &counts))
            .expect("buckets")
            .install_recorder()
            .expect("install prometheus recorder");
        for m in METRICS {
            match m.kind {
                Kind::Counter => metrics::describe_counter!(m.name, m.help),
                Kind::Gauge => metrics::describe_gauge!(m.name, m.help),
                Kind::Histogram => metrics::describe_histogram!(m.name, m.help),
            }
        }
        gauge!(BUILD_INFO, "version" => env!("CARGO_PKG_VERSION"), "git_sha" => git_sha()).set(1.0);
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

/// Called right before rendering a scrape.
pub fn collect_on_scrape() {
    if let Some(c) = PROCESS.get() {
        c.collect();
    }
    if let Some(s) = STARTED.get() {
        gauge!(UPTIME).set(s.elapsed().as_secs_f64());
    }
    if let Ok(h) = tokio::runtime::Handle::try_current() {
        let m = h.metrics();
        gauge!(TOKIO_WORKERS).set(m.num_workers() as f64);
        gauge!(TOKIO_ALIVE_TASKS).set(m.num_alive_tasks() as f64);
        gauge!(TOKIO_GLOBAL_QUEUE).set(m.global_queue_depth() as f64);
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

// ---------------------------------------------------------------------------
// Small helpers used by business code
// ---------------------------------------------------------------------------

pub fn argon2_duration(op: &'static str, d: Duration) {
    histogram!(ARGON2_DURATION, "op" => op).record(d.as_secs_f64());
}

pub fn http_error(kind: &'static str) {
    counter!(HTTP_ERRORS, "kind" => kind).increment(1);
}

/// Times a pipeline stage; records on drop and logs at TRACE inside the
/// current (request) span so logs line up with `jmp_stage_duration_seconds`.
pub struct Stage {
    name: &'static str,
    start: Instant,
}

impl Stage {
    pub fn start(name: &'static str) -> Self {
        Stage {
            name,
            start: Instant::now(),
        }
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let d = self.start.elapsed();
        histogram!(STAGE_DURATION, "stage" => self.name).record(d.as_secs_f64());
        tracing::trace!(stage = self.name, us = d.as_micros() as u64, "stage done");
    }
}

// ---------------------------------------------------------------------------
// Route templates (bounded label values)
// ---------------------------------------------------------------------------

const KNOWN_ROOTS: &[&str] = &[
    "system",
    "users",
    "useritems",
    "userviews",
    "userplayeditems",
    "userfavoriteitems",
    "items",
    "shows",
    "videos",
    "audio",
    "sessions",
    "playback",
    "branding",
    "quickconnect",
    "displaypreferences",
    "search",
    "genres",
    "musicgenres",
    "studios",
    "persons",
    "artists",
    "years",
    "livetv",
    "localization",
    "playingitems",
    "web",
    "socket",
    "embywebsocket",
    "syncplay",
    "plugins",
    "packages",
    "scheduledtasks",
    "metrics",
    "jmp",
    "movies",
    "trailers",
    "library",
    "playlists",
    "collections",
    "instantmix",
    "mediasegments",
    "trickplay",
    "lyrics",
    "favicon.ico",
    "robots.txt",
    "",
    "emby",
];
const NAME_ROOTS: &[&str] = &[
    "genres",
    "musicgenres",
    "studios",
    "persons",
    "artists",
    "years",
];

fn seg_template(seg: &str) -> String {
    if crate::ids::looks_like_id(seg) {
        return "{id}".into();
    }
    if !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
        return "{n}".into();
    }
    if let Some((base, ext)) = seg.rsplit_once('.') {
        let ext_l = ext.to_ascii_lowercase();
        let ext_t = if matches!(
            ext_l.as_str(),
            "m3u8" | "ts" | "mp4" | "m4s" | "vtt" | "srt" | "ass"
        ) {
            ext_l
        } else {
            "{ext}".into()
        };
        let b = seg_template(base);
        return format!("{b}.{ext_t}");
    }
    let l = seg.to_ascii_lowercase();
    if l.len() <= 24 && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        l
    } else {
        "{x}".into()
    }
}

/// Bounded route template for metrics labels, e.g. `/items/{id}/images/primary`.
pub fn route_template(path: &str) -> String {
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let segs: Vec<&str> = if segs.first().is_some_and(|s| s.eq_ignore_ascii_case("emby")) {
        segs[1..].to_vec()
    } else {
        segs
    };
    let Some(first) = segs.first() else {
        return "/".into();
    };
    let f = first.to_ascii_lowercase();
    if !KNOWN_ROOTS.contains(&f.as_str()) {
        return "/other".into();
    }
    if f == "web" {
        return "/web/*".into();
    }
    let mut out = String::new();
    for (i, s) in segs.iter().enumerate().take(7) {
        out.push('/');
        if i == 1 && NAME_ROOTS.contains(&f.as_str()) && !crate::ids::looks_like_id(s) {
            out.push_str("{name}");
        } else {
            out.push_str(&seg_template(s));
        }
    }
    if segs.len() > 7 {
        out.push_str("/...");
    }
    out
}

/// Classify a proxied path into a stream kind label.
pub fn stream_kind(path_lc: &str, content_type: &str) -> &'static str {
    if path_lc.contains("/images/") {
        "image"
    } else if path_lc.ends_with(".m3u8") || content_type.contains("mpegurl") {
        "hls_playlist"
    } else if path_lc.contains("/hls") || path_lc.ends_with(".ts") || path_lc.ends_with(".m4s") {
        "hls_segment"
    } else if path_lc.contains("/download") {
        "download"
    } else if path_lc.starts_with("/web") || path_lc.starts_with("web") {
        "web"
    } else if path_lc.contains("/stream")
        || path_lc.contains("/universal")
        || path_lc.contains("/audio/")
    {
        if path_lc.contains("static=true") || path_lc.contains("/universal") {
            "direct"
        } else {
            "transcode"
        }
    } else {
        "other"
    }
}

// ---------------------------------------------------------------------------
// HTTP middleware: request id + request metrics
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct RequestId(pub String);

fn valid_request_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

/// Outermost layer: assigns/propagates `X-Request-Id`, counts in-flight
/// requests and records request metrics.
pub async fn http_layer(mut req: Request, next: Next) -> Response {
    let rid = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| valid_request_id(s))
        .map(String::from)
        .unwrap_or_else(|| crate::auth::new_token()[..16].to_string());
    if let Ok(v) = HeaderValue::from_str(&rid) {
        req.headers_mut().insert("x-request-id", v);
    }
    req.extensions_mut().insert(RequestId(rid.clone()));
    let route = route_template(req.uri().path());
    let method = req.method().as_str().to_string();
    let g = gauge!(HTTP_IN_FLIGHT);
    g.increment(1.0);
    let start = Instant::now();
    let mut resp = next.run(req).await;
    g.decrement(1.0);
    let status = resp.status().as_u16().to_string();
    histogram!(HTTP_DURATION, "route" => route.clone(), "method" => method.clone())
        .record(start.elapsed().as_secs_f64());
    counter!(HTTP_REQUESTS, "route" => route.clone(), "method" => method, "status" => status)
        .increment(1);
    if let Some(len) = resp
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<f64>().ok())
    {
        histogram!(HTTP_RESPONSE_SIZE, "route" => route).record(len);
    }
    if let Ok(v) = HeaderValue::from_str(&rid) {
        resp.headers_mut().insert("x-request-id", v);
    }
    resp
}

/// Span for tower-http's TraceLayer.
pub fn make_span(req: &axum::http::Request<axum::body::Body>) -> tracing::Span {
    let rid = req
        .extensions()
        .get::<RequestId>()
        .map(|r| r.0.clone())
        .unwrap_or_default();
    tracing::info_span!(
        "http",
        request_id = %rid,
        method = %req.method(),
        route = %route_template(req.uri().path()),
    )
}

// ---------------------------------------------------------------------------
// Metered streaming bodies
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, serde::Serialize)]
pub struct StreamInfo {
    pub id: u64,
    pub server: String,
    pub kind: &'static str,
    pub route: String,
    pub client: String,
    pub started_unix: f64,
    pub bytes: u64,
}

static STREAM_SEQ: AtomicU64 = AtomicU64::new(1);
static ACTIVE_STREAMS: OnceLock<Mutex<HashMap<u64, StreamInfo>>> = OnceLock::new();

fn active() -> &'static Mutex<HashMap<u64, StreamInfo>> {
    ACTIVE_STREAMS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn active_streams() -> Vec<StreamInfo> {
    active().lock().unwrap().values().cloned().collect()
}

type BoxStream = crate::backend::BodyStream;

/// Wraps an upstream body: counts bytes, time to first byte, and how the
/// stream ended (complete / client disconnect / upstream error).
pub struct MeteredStream {
    inner: BoxStream,
    id: u64,
    server: String,
    kind: &'static str,
    started: Instant,
    first: bool,
    done: bool,
    errored: bool,
    pending_bytes: u64,
}

impl MeteredStream {
    pub fn new(
        inner: BoxStream,
        server: &str,
        kind: &'static str,
        route: String,
        client: String,
        started: Instant,
    ) -> Self {
        let id = STREAM_SEQ.fetch_add(1, Ordering::Relaxed);
        gauge!(STREAMS_ACTIVE, "server" => server.to_string(), "kind" => kind).increment(1.0);
        active().lock().unwrap().insert(
            id,
            StreamInfo {
                id,
                server: server.to_string(),
                kind,
                route,
                client,
                started_unix: unix_now(),
                bytes: 0,
            },
        );
        MeteredStream {
            inner,
            id,
            server: server.to_string(),
            kind,
            started,
            first: true,
            done: false,
            errored: false,
            pending_bytes: 0,
        }
    }

    fn flush_bytes(&mut self) {
        if self.pending_bytes > 0 {
            counter!(STREAM_BYTES, "server" => self.server.clone(), "kind" => self.kind)
                .increment(self.pending_bytes);
            if let Some(i) = active().lock().unwrap().get_mut(&self.id) {
                i.bytes += self.pending_bytes;
            }
            self.pending_bytes = 0;
        }
    }
}

impl Stream for MeteredStream {
    type Item = Result<Bytes, crate::error::BackendError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let r = self.inner.as_mut().poll_next(cx);
        match &r {
            Poll::Ready(Some(Ok(b))) => {
                if self.first {
                    self.first = false;
                    histogram!(STREAM_TTFB, "server" => self.server.clone(), "kind" => self.kind)
                        .record(self.started.elapsed().as_secs_f64());
                }
                self.pending_bytes += b.len() as u64;
                if self.pending_bytes >= 1 << 20 {
                    self.flush_bytes();
                }
            }
            Poll::Ready(Some(Err(_))) => self.errored = true,
            Poll::Ready(None) => self.done = true,
            Poll::Pending => {}
        }
        r
    }
}

impl Drop for MeteredStream {
    fn drop(&mut self) {
        self.flush_bytes();
        let outcome = if self.errored {
            "upstream_error"
        } else if self.done {
            "complete"
        } else {
            "client_disconnect"
        };
        gauge!(STREAMS_ACTIVE, "server" => self.server.clone(), "kind" => self.kind).decrement(1.0);
        counter!(STREAM_END, "server" => self.server.clone(), "kind" => self.kind, "outcome" => outcome).increment(1);
        active().lock().unwrap().remove(&self.id);
        tracing::debug!(server = %self.server, kind = self.kind, outcome, secs = self.started.elapsed().as_secs_f64(), "stream ended");
    }
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

use tracing_subscriber::{
    layer::SubscriberExt, reload, util::SubscriberInitExt, EnvFilter, Layer, Registry,
};

/// Handle to change the log filter at runtime.
#[derive(Clone)]
pub struct LogHandle {
    handle: reload::Handle<EnvFilter, Registry>,
    env_override: bool,
}

impl LogHandle {
    pub fn set_filter(&self, filter: &str) -> anyhow::Result<()> {
        if self.env_override {
            tracing::debug!("RUST_LOG is set; ignoring logging.filter from config");
            return Ok(());
        }
        let f = EnvFilter::try_new(filter)?;
        self.handle.reload(f)?;
        Ok(())
    }
}

pub fn init_logging(cfg: &crate::config::LoggingConfig) -> LogHandle {
    use crate::config::LogFormat;
    let env = std::env::var("RUST_LOG").ok().filter(|s| !s.is_empty());
    let filter = env
        .as_deref()
        .and_then(|s| EnvFilter::try_new(s).ok())
        .unwrap_or_else(|| {
            EnvFilter::try_new(&cfg.filter).unwrap_or_else(|_| EnvFilter::new("info"))
        });
    let (filter_layer, handle) = reload::Layer::new(filter);
    let ansi = cfg.ansi.unwrap_or(cfg.format != LogFormat::Json);
    type L = Box<
        dyn Layer<tracing_subscriber::layer::Layered<reload::Layer<EnvFilter, Registry>, Registry>>
            + Send
            + Sync,
    >;
    let fmt_layer: L = match cfg.format {
        LogFormat::Pretty => tracing_subscriber::fmt::layer().with_ansi(ansi).boxed(),
        LogFormat::Compact => tracing_subscriber::fmt::layer()
            .compact()
            .with_ansi(ansi)
            .boxed(),
        LogFormat::Json => tracing_subscriber::fmt::layer()
            .json()
            .with_current_span(true)
            .with_span_list(false)
            .with_ansi(false)
            .boxed(),
    };
    let _ = tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .try_init();
    LogHandle {
        handle,
        env_override: env.is_some(),
    }
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
