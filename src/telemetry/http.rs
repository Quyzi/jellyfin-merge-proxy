//! Route templates (bounded label values) and the HTTP request middleware.

use super::*;

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

/// HTTP status class label (`2xx` … `5xx`); bounded for aggregations.
pub fn status_class(code: u16) -> &'static str {
    match code {
        100..=199 => "1xx",
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "other",
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
    let code = resp.status().as_u16();
    let status = code.to_string();
    let class = status_class(code);
    histogram!(HTTP_DURATION, "route" => route.clone(), "method" => method.clone())
        .record(start.elapsed().as_secs_f64());
    // Labels stay bounded: route templates, HTTP methods, and ~40 status codes.
    counter!(
        HTTP_REQUESTS,
        "route" => route.clone(),
        "method" => method,
        "status" => status,
        "status_class" => class
    )
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
