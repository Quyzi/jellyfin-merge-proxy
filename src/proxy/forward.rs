//! Routed requests: forwarding to the owning upstream and relaying the response.

use super::*;

// ---------------------------------------------------------------------------
// Routed requests
// ---------------------------------------------------------------------------

pub async fn playstate(st: &Shared, r: Req, c: &Client) -> Result<Response> {
    let body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
    let item = body
        .get("ItemId")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| q_get(&r.query, "ItemId").map(String::from));
    if let Some(id) = item {
        record_playback(st, &r, c, &id, &body);
        if let Some(Target::Item { upstream, .. }) = resolve_or_refresh(st, &id).await {
            return forward(st, upstream, r, c, None).await;
        }
    }
    tracing::debug!("playstate report for unknown item, ignoring");
    Ok(no_content())
}

/// Update the proxy's own play state from a playback report.
fn record_playback(st: &Shared, r: &Req, c: &Client, id: &str, body: &Value) {
    use crate::userdata::service::PlayEvent;
    if !st.userdata.enabled() || c.user_id.is_empty() {
        return;
    }
    let Some(ev) = r.segs.last().and_then(|s| PlayEvent::from_seg(s)) else {
        return;
    };
    let Some(pid) = crate::types::ProxyItemId::parse(id) else {
        return;
    };
    let pos = body
        .get("PositionTicks")
        .and_then(|v| v.as_i64())
        .or_else(|| q_get(&r.query, "PositionTicks").and_then(|v| v.parse().ok()));
    st.userdata
        .playback(&c.user_id, &pid, ev, pos, &crate::dto::now_iso());
}

/// Find the owning upstream of a request by the first proxy id in its path,
/// then its query string.
pub(super) async fn owner_of(st: &Shared, r: &Req) -> Option<ServerId> {
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
                Ok(ok_json(dto::query_result(vec![], 0, 0)))
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

pub(super) const FWD_REQ_HEADERS: &[&str] = &[
    "range",
    "if-range",
    "if-none-match",
    "if-modified-since",
    "accept",
    "accept-language",
    "content-type",
    "user-agent",
];

pub(super) const FWD_RESP_HEADERS: &[&str] = &[
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

/// Forward a request to upstream `u`, translating ids both ways.
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
    let uc = match up.login_as(Some(&c.user_id)).await {
        Ok(uc) => uc,
        Err(e) => {
            tracing::warn!(upstream = %up.name(), "{e}");
            return Ok(err_resp(
                StatusCode::SERVICE_UNAVAILABLE,
                "upstream unavailable",
            ));
        }
    };
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
    // Keep a trailing slash: Jellyfin redirects `/web` to `/web/`, so
    // dropping it made `/web/` redirect to itself forever.
    let pq = path_with_query_slash(&segs, &q, trailing_slash);
    // Never log `pq` itself at info: it may carry ids; tokens were stripped above.
    tracing::debug!(upstream = %up.name(), method = %r.method, route = %telemetry::route_template(&segs.join("/")), "forwarding");
    if headers.contains_key(header::RANGE) {
        counter!(names::RANGE_REQUESTS, "server" => up.name().to_string()).increment(1);
    }
    let started = Instant::now();
    let req = BackendRequest::new(r.method.clone(), pq)
        .headers(headers)
        .body(body)
        .streaming(true)
        .as_user(Some(&c.user_id));
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

/// Send a backend response to the client through the rewriter chain.
/// JSON and HLS playlists are buffered and rewritten; everything else is
/// streamed (only header rewriters run).
pub(super) async fn relay(
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
        // Rewritten bodies get fresh headers: only the content type is kept
        // (lengths, encodings and validators no longer apply).
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
            // Unparseable JSON: pass through untouched.
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
            Some(PayloadBody::Json(v)) => json_resp(status, &v),
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
            // json_resp already set a JSON content type.
            if !(json_body && k == header::CONTENT_TYPE) {
                res.headers_mut().insert(k.clone(), v.clone());
            }
        }
        return Ok(res);
    }
    // Stream everything else through without buffering.
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
