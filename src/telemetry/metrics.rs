//! Metric definitions and metadata.

use super::names::*;

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
    m!(SECRETS_REWRITTEN, Counter, "kind", "Secrets rewritten in config file: user_password, upstream_password, upstream_api_key."),
    m!(SECRET_REWRITE_FAILURES, Counter, "", "Config file secret rewrite failures (permission/IO errors)."),
    m!(SECRET_DECRYPT_FAILURES, Counter, "kind", "Secret decryption failures during config load (wrong key or corrupted data); kind: upstream_password, upstream_api_key."),
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
