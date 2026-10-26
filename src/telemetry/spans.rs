//! HTTP tracing and route template generation.

use super::names::*;
use axum::extract::Request;
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;
use metrics::{counter, gauge, histogram};
use std::time::{Duration, Instant};

pub fn argon2_duration(op: &'static str, d: Duration) {
    histogram!(ARGON2_DURATION, "op" => op).record(d.as_secs_f64());
}

pub fn http_error(kind: &'static str) {
    counter!(HTTP_ERRORS, "kind" => kind).increment(1);
}

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

#[derive(Clone, Debug)]
pub struct RequestId(pub String);

fn valid_request_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

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
