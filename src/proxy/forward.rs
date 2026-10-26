use anyhow::Result;
use axum::body::{Body, Bytes};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::Response;
use metrics::counter;
use serde_json::Value;
use std::time::Instant;

use super::catalog::resolve_or_refresh;
use super::response::{err_resp, is_token_key, no_content, ok_json};
use super::{Client, Req};
use crate::backend::{BackendRequest, BackendResponse, MediaBackend, SessionInfo};
use crate::error::BackendError;
use crate::ids::{looks_like_id, normalize_hex_id, Target};
use crate::rewrite::{self, InCtx, OutCtx, Payload, PayloadBody};
use crate::servers::ServerState;
use crate::telemetry::{self, names, Stage};
use crate::types::ServerId;
use crate::Shared;

pub fn out_ctx<'a>(
    st: &'a Shared,
    up: &'a dyn MediaBackend,
    uc: &'a SessionInfo,
    c: &'a Client,
) -> OutCtx<'a> {
    let loc = up.location();
    OutCtx {
        ids: st.ids.as_ref(),
        upstream: up.id(),
        server_id: &st.server_id,
        proxy_user_id: &c.user_id,
        upstream_user_id: uc.user_id.as_deref(),
        client_token: &c.token,
        upstream_origin: &loc.origin,
        upstream_base_path: &loc.base_path,
    }
}

fn in_ctx<'a>(
    st: &'a Shared,
    up: &'a dyn MediaBackend,
    uc: &'a SessionInfo,
    user_ids: &'a [String],
) -> InCtx<'a> {
    InCtx {
        ids: st.ids.as_ref(),
        upstream: up.id(),
        upstream_user_id: uc.user_id.as_deref(),
        proxy_user_ids: user_ids,
    }
}

pub fn path_with_query(segs: &[String], q: &[(String, String)]) -> String {
    path_with_query_slash(segs, q, false)
}

fn path_with_query_slash(segs: &[String], q: &[(String, String)], slash: bool) -> String {
    let mut s = segs.join("/");
    if slash && !s.is_empty() {
        s.push('/');
    }
    if !q.is_empty() {
        s.push('?');
        s.push_str(&rewrite::encode_pairs(q));
    }
    s
}

pub fn classify(st: &Shared, seg: &str) -> String {
    if let Some(n) = normalize_hex_id(seg) {
        if st.is_proxy_user(&n) {
            return "{u}".into();
        }
        if n == st.root_id {
            return "{root}".into();
        }
        if let Some(Target::View(_)) = st.ids.resolve(&n) {
            return "{view}".into();
        }
        return "{id}".into();
    }
    seg.to_ascii_lowercase()
}

pub async fn playstate(st: &Shared, r: Req, c: &Client) -> Result<Response> {
    let body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
    let item = body
        .get("ItemId")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| super::response::q_get(&r.query, "ItemId").map(String::from));
    if let Some(id) = item {
        if let Some(Target::Item { upstream, .. }) = resolve_or_refresh(st, &id).await {
            return forward(st, upstream, r, c, None).await;
        }
    }
    tracing::debug!("playstate report for unknown item, ignoring");
    Ok(no_content())
}

async fn owner_of(st: &Shared, r: &Req) -> Option<ServerId> {
    let _s = Stage::start("id_decode");
    let pick = |t: Target| match t {
        Target::Item { upstream, .. } => Some(upstream),
        Target::View(v) => v.members.first().map(|(u, _)| *u),
    };
    let mut unresolved = vec![];
    for s in &r.segs {
        if let Some(n) = normalize_hex_id(s) {
            if st.is_proxy_user(&n) {
                continue;
            }
            match st.ids.resolve(&n) {
                Some(t) => return pick(t),
                None => unresolved.push(n),
            }
        }
    }
    for (k, v) in &r.query {
        if is_token_key(k) {
            continue;
        }
        for part in v.split(',') {
            if let Some(t) = normalize_hex_id(part).and_then(|n| st.ids.resolve(&n)) {
                return pick(t);
            }
        }
    }
    if let Some(n) = unresolved.first() {
        if let Some(t) = resolve_or_refresh(st, n).await {
            return pick(t);
        }
    }
    None
}

pub async fn generic(st: &Shared, r: Req, c: &Client) -> Result<Response> {
    if let Some(u) = owner_of(st, &r).await {
        return forward(st, u, r, c, None).await;
    }
    let has_unknown_id = r
        .segs
        .iter()
        .any(|s| normalize_hex_id(s).is_some_and(|n| !st.is_proxy_user(&n)));
    if has_unknown_id {
        tracing::info!(method = %r.method, route = %telemetry::route_template(&r.raw_path), "unknown item id");
        return Ok(err_resp(StatusCode::NOT_FOUND, "Item not found"));
    }
    match st.cfg().proxy.unknown_routes {
        crate::config::UnknownRoutes::Primary => {
            tracing::info!(method = %r.method, route = %telemetry::route_template(&r.raw_path), "unhandled route -> primary upstream");
            primary_forward(st, r, c).await
        }
        crate::config::UnknownRoutes::Empty => {
            tracing::info!(method = %r.method, route = %telemetry::route_template(&r.raw_path), "unhandled route -> empty result");
            if r.method == Method::GET {
                Ok(ok_json(crate::dto::query_result(vec![], 0, 0)))
            } else {
                Ok(no_content())
            }
        }
    }
}

pub async fn primary_forward(st: &Shared, r: Req, c: &Client) -> Result<Response> {
    let set = st.backends();
    let Some(up) = set.iter().find(|u| !u.is_down()).or(set.first()) else {
        return Ok(err_resp(StatusCode::SERVICE_UNAVAILABLE, "no upstream"));
    };
    forward(st, up.id(), r, c, None).await
}

const FWD_REQ_HEADERS: &[&str] = &[
    "range",
    "if-range",
    "if-none-match",
    "if-modified-since",
    "accept",
    "accept-language",
    "content-type",
    "user-agent",
];

const FWD_RESP_HEADERS: &[&str] = &[
    "content-type",
    "content-length",
    "content-range",
    "accept-ranges",
    "etag",
    "last-modified",
    "cache-control",
    "content-disposition",
    "content-encoding",
    "expires",
    "vary",
];

pub async fn forward(
    st: &Shared,
    u: ServerId,
    r: Req,
    c: &Client,
    segs_override: Option<Vec<String>>,
) -> Result<Response> {
    let up = match st.server_state(u) {
        ServerState::Active(b) => b,
        ServerState::Removed(name) => {
            counter!(names::REMOVED_SERVER_REQUESTS).increment(1);
            tracing::debug!(upstream = %name, "request for an item of a removed server");
            return Ok(err_resp(StatusCode::GONE, "server removed"));
        }
        ServerState::Unknown => {
            return Ok(err_resp(StatusCode::SERVICE_UNAVAILABLE, "no upstream"));
        }
    };
    if let Err(e) = up.ensure_login().await {
        tracing::warn!(upstream = %up.name(), "{e}");
        return Ok(err_resp(
            StatusCode::SERVICE_UNAVAILABLE,
            "upstream unavailable",
        ));
    }
    let uc = up.session().await;
    let user_ids = st.authenticator.ids();
    let ic = in_ctx(st, up.as_ref(), &uc, &user_ids);
    let segs_override_none = segs_override.is_none();
    let segs: Vec<String> = segs_override
        .unwrap_or_else(|| r.segs.clone())
        .iter()
        .map(|s| {
            if looks_like_id(s) {
                ic.map(s)
            } else {
                s.clone()
            }
        })
        .collect();
    let mut q: Vec<(String, String)> = r
        .query
        .iter()
        .filter(|(k, _)| !is_token_key(k))
        .map(|(k, v)| (k.clone(), ic.map_list(v)))
        .collect();
    let trailing_slash = segs_override_none && r.raw_path.len() > 1 && r.raw_path.ends_with('/');
    let segs = crate::paths::adapt(segs, &mut q, uc.modern, uc.user_id.as_deref());
    let mut headers = HeaderMap::new();
    for h in FWD_REQ_HEADERS {
        if let Some(v) = r.headers.get(*h) {
            headers.insert(*h, v.clone());
        }
    }
    let body = if r.body.is_empty() {
        None
    } else {
        let is_json = r
            .headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("json"));
        match (is_json, serde_json::from_slice::<Value>(&r.body)) {
            (true, Ok(mut v)) => {
                rewrite::json_in(&mut v, &ic);
                Some(Bytes::from(serde_json::to_vec(&v)?))
            }
            _ => Some(r.body.clone()),
        }
    };
    let pq = path_with_query_slash(&segs, &q, trailing_slash);
    tracing::debug!(upstream = %up.name(), method = %r.method, route = %telemetry::route_template(&segs.join("/")), "forwarding");
    if headers.contains_key(header::RANGE) {
        counter!(names::RANGE_REQUESTS, "server" => up.name().to_string()).increment(1);
    }
    let started = Instant::now();
    let req = BackendRequest::new(r.method.clone(), pq)
        .headers(headers)
        .body(body)
        .streaming(true);
    let resp = match up.send(req).await {
        Ok(resp) => resp,
        Err(e @ BackendError::Down { .. }) => {
            tracing::warn!(upstream = %up.name(), "{e}");
            return Ok(err_resp(
                StatusCode::SERVICE_UNAVAILABLE,
                "upstream unavailable",
            ));
        }
        Err(e) => return Err(e.into()),
    };
    relay(st, up.as_ref(), &uc, c, resp, &r, started).await
}

async fn relay(
    st: &Shared,
    up: &dyn MediaBackend,
    uc: &SessionInfo,
    c: &Client,
    resp: BackendResponse,
    r: &Req,
    started: Instant,
) -> Result<Response> {
    let status = resp.status;
    let ctype = resp
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let ctx = out_ctx(st, up, uc, c);
    let path = r.raw_path.to_ascii_lowercase();
    let is_playlist = rewrite::is_playlist(&ctype, &path);
    let buffer = r.method != Method::HEAD && (ctype.contains("application/json") || is_playlist);
    let mut headers = HeaderMap::new();
    if buffer {
        if let Some(v) = resp.headers.get(header::CONTENT_TYPE) {
            headers.insert(header::CONTENT_TYPE, v.clone());
        }
    } else {
        for h in FWD_RESP_HEADERS {
            if let Some(v) = resp.headers.get(*h) {
                headers.insert(*h, v.clone());
            }
        }
    }
    if let Some(v) = resp.headers.get(header::LOCATION) {
        headers.insert(header::LOCATION, v.clone());
    }
    if buffer {
        let bytes = resp.bytes().await?;
        let _s = Stage::start("rewrite");
        let body = if is_playlist {
            Some(PayloadBody::Text(
                String::from_utf8_lossy(&bytes).into_owned(),
            ))
        } else {
            serde_json::from_slice::<Value>(&bytes)
                .ok()
                .map(PayloadBody::Json)
        };
        let Some(body) = body else {
            let mut res = Response::new(Body::from(bytes));
            *res.status_mut() = status;
            if let Some(v) = headers.get(header::CONTENT_TYPE) {
                res.headers_mut().insert(header::CONTENT_TYPE, v.clone());
            }
            return Ok(res);
        };
        let mut p = Payload {
            content_type: ctype.clone(),
            path,
            headers: &mut headers,
            body: Some(body),
        };
        let ran = st.rewriters.apply(&mut p, &ctx);
        if ran.contains(&"hls") {
            counter!(names::HLS_PLAYLIST_REWRITES, "server" => up.name().to_string()).increment(1);
        }
        let mut res = match p.body.take() {
            Some(PayloadBody::Json(v)) => super::response::json_resp(status, &v),
            Some(PayloadBody::Text(t)) => {
                let mut res = Response::new(Body::from(t));
                *res.status_mut() = status;
                res
            }
            None => {
                let mut res = Response::new(Body::empty());
                *res.status_mut() = status;
                res
            }
        };
        let json_body = res.headers().contains_key(header::CONTENT_TYPE);
        for (k, v) in headers.iter() {
            if !(json_body && k == header::CONTENT_TYPE) {
                res.headers_mut().insert(k.clone(), v.clone());
            }
        }
        return Ok(res);
    }
    let mut p = Payload {
        content_type: ctype.clone(),
        path: path.clone(),
        headers: &mut headers,
        body: None,
    };
    st.rewriters.apply(&mut p, &ctx);
    let mut builder = Response::builder().status(status);
    for (k, v) in headers.iter() {
        builder = builder.header(k, v);
    }
    let body = if r.method == Method::HEAD {
        Body::empty()
    } else {
        let mut key = path;
        if r.query
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("static") && v.eq_ignore_ascii_case("true"))
        {
            key.push_str("?static=true");
        }
        let kind = telemetry::stream_kind(&key, &ctype);
        if kind == "hls_segment" {
            counter!(names::HLS_SEGMENT_REQUESTS, "server" => up.name().to_string()).increment(1);
        }
        let who = c
            .session
            .as_ref()
            .map(|s| format!("{} / {} ({})", s.user_name, s.client, s.device_name))
            .unwrap_or_else(|| "anonymous".into());
        let metered = telemetry::MeteredStream::new(
            resp.body,
            up.name(),
            kind,
            telemetry::route_template(&r.raw_path),
            who,
            started,
        );
        Body::from_stream(metered)
    };
    Ok(builder.body(body)?)
}
