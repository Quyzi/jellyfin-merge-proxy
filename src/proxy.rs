//! HTTP surface: request parsing, the shared building blocks used by the
//! endpoint modules (views, fan-out lists, forwarding/relay), metrics,
//! debug and WebSocket handlers. Route selection lives in
//! [`crate::endpoints`].

use crate::auth::{self, Session};
use crate::backend::{BackendRequest, BackendResponse, MediaBackend, SessionInfo};
use crate::cache::{CacheKey, Lookup};
use crate::dto;
use crate::error::BackendError;
use crate::ids::{hash_id, looks_like_id, normalize_hex_id, synthetic_id, MergedView, Target};
use crate::merge::{self, Deduplicator, MeteredDeduplicator, Ranked, ViewGroup};
use crate::password::Password;
use crate::rewrite::{self, InCtx, OutCtx, Payload, PayloadBody};
use crate::servers::ServerState;
use crate::telemetry::{self, names, Stage};
use crate::types::{ServerId, UpstreamItemId};
use crate::Shared;
use anyhow::Result;
use axum::body::{Body, Bytes};
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use futures_util::future::join_all;
use metrics::{counter, histogram};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;

const MAX_BODY: usize = 64 * 1024 * 1024;

pub fn router(st: Shared) -> Router {
    let cfg = st.cfg();
    let mut r = Router::new()
        .route("/socket", any(ws_handler))
        .route("/embywebsocket", any(ws_handler));
    if cfg.metrics.enabled && cfg.metrics.listen.is_none() {
        r = r.route(&cfg.metrics.path, axum::routing::get(metrics_handler));
    }
    if cfg.debug.enabled {
        r = r.route(&cfg.debug.path, axum::routing::get(debug_handler));
    }
    r.fallback(dispatch)
        .layer(tower_http::cors::CorsLayer::very_permissive())
        .layer(
            tower_http::trace::TraceLayer::new_for_http()
                .make_span_with(telemetry::make_span)
                .on_request(())
                .on_response(
                    tower_http::trace::DefaultOnResponse::new()
                        .level(tracing::Level::DEBUG)
                        .latency_unit(tower_http::LatencyUnit::Millis),
                )
                .on_failure(tower_http::trace::DefaultOnFailure::new().level(tracing::Level::WARN)),
        )
        .layer(axum::middleware::from_fn(telemetry::http_layer))
        .with_state(st)
}

/// Router with only the metrics endpoint (for `metrics.listen`).
pub fn metrics_router(st: Shared) -> Router {
    let path = st.cfg().metrics.path.clone();
    Router::new()
        .route(&path, axum::routing::get(metrics_handler))
        .with_state(st)
}

fn session_from(st: &Shared, headers: &HeaderMap, query: Option<&str>) -> Option<Session> {
    let q: Vec<(String, String)> = query
        .map(|q| {
            form_urlencoded::parse(q.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default();
    let info = auth::client_auth(headers, &q);
    info.get("token").and_then(|t| st.sessions.get(t))
}

/// Parsed client request.
pub struct Req {
    pub method: Method,
    pub segs: Vec<String>,
    pub query: Vec<(String, String)>,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub raw_path: String,
}

/// The authenticated (or anonymous, for images) client.
#[derive(Clone)]
pub struct Client {
    pub token: String,
    pub user_id: String,
    pub session: Option<Session>,
}

pub fn q_get<'a>(q: &'a [(String, String)], key: &str) -> Option<&'a str> {
    q.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty())
}

fn is_token_key(k: &str) -> bool {
    k.eq_ignore_ascii_case("api_key") || k.eq_ignore_ascii_case("apikey")
}

pub fn json_resp(status: StatusCode, v: &Value) -> Response {
    let _s = Stage::start("serialize");
    let mut r = Response::new(Body::from(serde_json::to_vec(v).unwrap_or_default()));
    *r.status_mut() = status;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    r
}

pub fn ok_json(v: Value) -> Response {
    json_resp(StatusCode::OK, &v)
}

pub fn no_content() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

pub fn err_resp(status: StatusCode, msg: &str) -> Response {
    (status, msg.to_string()).into_response()
}

async fn dispatch(State(st): State<Shared>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let raw_path = parts.uri.path().to_string();
    let mut segs: Vec<String> = raw_path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if segs
        .first()
        .is_some_and(|s| s.eq_ignore_ascii_case("emby") || s.eq_ignore_ascii_case("mediabrowser"))
    {
        segs.remove(0);
    }
    let query: Vec<(String, String)> = parts
        .uri
        .query()
        .map(|q| {
            form_urlencoded::parse(q.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default();
    let body = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(_) => {
            telemetry::http_error("payload_too_large");
            return err_resp(StatusCode::PAYLOAD_TOO_LARGE, "body too large");
        }
    };
    let r = Req {
        method: parts.method,
        segs,
        query,
        headers: parts.headers,
        body,
        raw_path,
    };
    let method = r.method.clone();
    let route = telemetry::route_template(&r.raw_path);
    let resp = match st.endpoints.dispatch(&st, r).await {
        Ok(resp) => resp,
        Err(e) => {
            // Paths can carry ids; log the template only.
            tracing::warn!(%method, %route, "request failed: {e:#}");
            telemetry::http_error("upstream_error");
            err_resp(StatusCode::BAD_GATEWAY, "upstream error")
        }
    };
    match resp.status() {
        StatusCode::UNAUTHORIZED => telemetry::http_error("unauthorized"),
        StatusCode::NOT_FOUND => telemetry::http_error("not_found"),
        StatusCode::SERVICE_UNAVAILABLE => telemetry::http_error("upstream_unavailable"),
        _ => {}
    }
    resp
}

pub fn host_address(h: &HeaderMap) -> String {
    let host = h
        .get("x-forwarded-host")
        .or_else(|| h.get(header::HOST))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost:8096");
    let proto = h
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("http");
    format!("{proto}://{host}")
}

/// Classify a segment for route matching.
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

/// Resolve the client for an authenticated route. GET requests for images
/// may be anonymous (clients load them via `<img>`, as with Jellyfin).
pub fn authenticate(st: &Shared, r: &Req, classified: &[&str]) -> Option<Client> {
    let _s = Stage::start("auth");
    let is_get = r.method == Method::GET || r.method == Method::HEAD;
    // `Items/{id}/Images` (the image-info list) needs a token like Jellyfin's
    // [Authorize]; the image binaries below it stay anonymous.
    let anonymous_ok = is_get
        && (classified
            .iter()
            .position(|s| *s == "images")
            .is_some_and(|i| i + 1 < classified.len())
            || classified.first() == Some(&"userimage")
            || classified == ["branding", "splashscreen"]);
    let info = auth::client_auth(&r.headers, &r.query);
    let token = info.get("token");
    let session = token.and_then(|t| st.sessions.get(t));
    let outcome = match (&session, token) {
        (Some(_), _) => "valid",
        (None, Some(_)) if anonymous_ok => "anonymous",
        (None, Some(_)) => "invalid",
        (None, None) if anonymous_ok => "anonymous",
        (None, None) => "missing",
    };
    counter!(names::TOKEN_VALIDATIONS, "outcome" => outcome).increment(1);
    if let Some(s) = &session {
        st.local.live.touch(s);
    }
    match session {
        Some(s) => Some(Client {
            token: s.token.clone(),
            user_id: s.user_id.clone(),
            session: Some(s),
        }),
        None if anonymous_ok => Some(anon_client(st)),
        None => None,
    }
}

pub fn anon_client(st: &Shared) -> Client {
    Client {
        token: String::new(),
        user_id: st.authenticator.ids().first().cloned().unwrap_or_default(),
        session: None,
    }
}

pub fn current_user(st: &Shared, user_id: &str) -> Value {
    let name = st
        .authenticator
        .by_id(user_id)
        .map(|u| u.name)
        .unwrap_or_default();
    dto::user_dto(
        &name,
        user_id,
        &st.server_id,
        &st.cfg().proxy.name,
        st.local.settings.get(user_id),
        crate::local::policy(st, user_id),
    )
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn login(st: &Shared, r: &Req) -> Response {
    let body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
    let field = |names: &[&str]| -> Option<String> {
        body.as_object().and_then(|o| {
            o.iter()
                .find(|(k, _)| names.iter().any(|n| k.eq_ignore_ascii_case(n)))
                .and_then(|(_, v)| v.as_str().map(String::from))
        })
    };
    let denied = || err_resp(StatusCode::UNAUTHORIZED, "Invalid username or password");
    let Some(username) = field(&["Username", "Name"]) else {
        // The authenticator never sees this one, so count it here.
        counter!(names::LOGINS, "outcome" => "bad_request").increment(1);
        return denied();
    };
    let pw = Password::new(field(&["Pw", "Password"]).unwrap_or_default());
    // argon2 verification is deliberately slow: run it off the async
    // workers. The (metered) authenticator counts the outcome.
    let auth = st.authenticator.clone();
    let name = username.clone();
    let res = tokio::task::spawn_blocking(move || auth.authenticate(&name, &pw)).await;
    let user = match res {
        Ok(Ok(u)) => u,
        Ok(Err(e)) => {
            tracing::info!(user = %username, reason = e.reason(), "login failed");
            return denied();
        }
        Err(_) => return denied(),
    };
    let info = auth::client_auth(&r.headers, &[]);
    let token = auth::new_token();
    let s = Session {
        token: token.clone(),
        session_id: hash_id(&["session", &token]),
        user_name: user.name.clone(),
        user_id: user.id.clone(),
        client: info
            .get("client")
            .cloned()
            .unwrap_or_else(|| "Unknown".into()),
        device_name: info
            .get("device")
            .cloned()
            .unwrap_or_else(|| "Unknown".into()),
        device_id: info
            .get("deviceid")
            .cloned()
            .unwrap_or_else(|| hash_id(&["device", &token])),
        app_version: info.get("version").cloned().unwrap_or_default(),
        created: dto::now_iso(),
    };
    tracing::info!(user = %s.user_name, client = %s.client, device = %s.device_name, "user logged in");
    // Jellyfin logs the same user out of this device before issuing a token.
    for old in st
        .sessions
        .remove_where(|x| x.user_id == user.id && x.device_id == s.device_id)
    {
        st.local.live.remove(&old.session_id);
    }
    st.sessions.insert(s.clone());
    st.local.live.touch(&s);
    let info = st
        .local
        .live
        .dto_of(&s.session_id, &st.server_id)
        .unwrap_or_default();
    let user_dto = current_user(st, &user.id);
    ok_json(dto::auth_result(user_dto, info, &s.token, &st.server_id))
}

mod admin;
mod client_fields;
mod context;
mod fan;
mod fetch;
mod forward;
mod indexed;
mod indexed_counts;
mod indexed_names;
mod indexed_user;
mod list;
mod merged;
mod playlists;
mod siblings;
mod specs;
mod views;
mod ws;

use admin::*;
use context::*;
pub(crate) use context::{in_ctx, out_ctx, path_with_query};
pub use fan::*;
pub(crate) use fetch::fetch_dtos;
pub use forward::*;
pub use indexed::*;
pub use indexed_counts::*;
pub use indexed_names::*;
pub use indexed_user::*;
pub(crate) use list::upstream_json;
pub use list::*;
pub use merged::*;
pub use playlists::*;
pub use siblings::*;
pub use specs::*;
pub use views::*;
use ws::*;

#[cfg(test)]
mod tests;
