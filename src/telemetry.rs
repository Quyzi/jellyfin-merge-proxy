//! Logging and metrics.
//!
//! All metric names live in [`names`].
//! Label values are always bounded: route templates, upstream names, and
//! small enums - never raw paths, item ids, or tokens.

use metrics::{counter, histogram};
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

use names::*;

pub fn git_sha() -> &'static str {
    option_env!("JMP_GIT_SHA").unwrap_or("unknown")
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
