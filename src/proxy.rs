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
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
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

async fn metrics_handler(State(st): State<Shared>, req: Request) -> Response {
    if st.cfg().metrics.require_auth
        && session_from(&st, req.headers(), req.uri().query()).is_none()
    {
        return err_resp(StatusCode::UNAUTHORIZED, "Unauthorized");
    }
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        telemetry::render(),
    )
        .into_response()
}

/// Config-gated JSON dump of internal state. Never contains tokens or
/// passwords (sessions are shown with a short hash fingerprint).
async fn debug_handler(State(st): State<Shared>, req: Request) -> Response {
    let cfg = st.cfg();
    let Some(sess) = session_from(&st, req.headers(), req.uri().query()) else {
        return err_resp(StatusCode::UNAUTHORIZED, "Unauthorized");
    };
    if !cfg.debug.users.is_empty()
        && !cfg
            .debug
            .users
            .iter()
            .any(|u| u.eq_ignore_ascii_case(&sess.user_name))
    {
        return err_resp(StatusCode::FORBIDDEN, "Forbidden");
    }
    let mut ups = vec![];
    let set = st.backends();
    for u in set.iter() {
        let mut v = serde_json::to_value(u.status().await).unwrap_or(Value::Null);
        if let Value::Object(o) = &mut v {
            o.insert("server_id".into(), json!(u.id().0));
            o.insert("in_flight".into(), json!(u.in_flight()));
        }
        ups.push(v);
    }
    let removed: Vec<String> = st
        .servers
        .names()
        .into_iter()
        .filter(|n| set.by_name(n).is_none())
        .collect();
    let settings = st.catalog.settings();
    let catalog = json!({
        "impl": st.catalog.name(),
        "entries": st.catalog.len(),
        "ttl_secs": settings.ttl.as_secs(),
        "max_entries": settings.max_entries,
    });
    let last_reload = st.last_reload.lock().unwrap().clone();
    ok_json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "git_sha": telemetry::git_sha(),
        "uptime_secs": st.started.elapsed().as_secs(),
        "server_id": st.server_id,
        "config": {
            "generation": st.generation.load(std::sync::atomic::Ordering::Relaxed),
            "source": st.source.as_ref().map(|s| s.describe()),
            "last_reload": last_reload.map(|(t, e)| json!({"at": t, "error": e})),
            "users": st.authenticator.list().into_iter().map(|u| u.name).collect::<Vec<_>>(),
            "merge": format!("{:?}", cfg.merge),
            "tls_listener": cfg.tls.is_some(),
        },
        "upstreams": ups,
        "removed_servers": removed,
        "id_map": {
            "total": st.ids.len(),
            "per_server": st.ids.sizes().into_iter().map(|(n, c)| json!({"server": n, "entries": c})).collect::<Vec<_>>(),
        },
        "caches": {
            "catalog": catalog,
            "display_prefs_entries": st.display_prefs.lock().unwrap().len(),
            "password_hashes": st.password_cache().len(),
        },
        "sessions": st.sessions.list(),
        "active_streams": telemetry::active_streams(),
        "architecture": {
            "endpoints": st.endpoints.names(),
            "rewriters": st.rewriters.names(),
            "reload_subscribers": st.hub.names(),
            "authenticator": st.authenticator.name(),
            "tls_provider": st.tls_provider.name(),
            "merge_strategy": merge::strategy_for(&cfg.merge).name(),
        },
    }))
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
    let anonymous_ok = is_get && classified.contains(&"images");
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

pub fn current_user(st: &Shared, c: &Client) -> Value {
    let name = st
        .authenticator
        .by_id(&c.user_id)
        .map(|u| u.name)
        .unwrap_or_default();
    dto::user_dto(&name, &c.user_id, &st.server_id, &st.cfg().proxy.name)
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
    st.sessions.insert(s.clone());
    let user_dto = dto::user_dto(&user.name, &user.id, &st.server_id, &st.cfg().proxy.name);
    ok_json(dto::auth_result(user_dto, &s, &st.server_id))
}

pub fn display_prefs(st: &Shared, r: &Req, c: &Client) -> Response {
    let id = r.segs[1].clone();
    let client = q_get(&r.query, "client").unwrap_or("emby").to_string();
    let key = format!("{}|{}|{}", c.user_id, id, client);
    if r.method == Method::POST {
        if let Ok(v) = serde_json::from_slice::<Value>(&r.body) {
            st.display_prefs.lock().unwrap().insert(key, v);
        }
        return no_content();
    }
    let v = st
        .display_prefs
        .lock()
        .unwrap()
        .get(&key)
        .cloned()
        .unwrap_or_else(|| dto::display_prefs(&id, &client));
    ok_json(v)
}

// ---------------------------------------------------------------------------
// Rewriting contexts
// ---------------------------------------------------------------------------

fn out_ctx<'a>(
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

fn path_with_query(segs: &[String], q: &[(String, String)]) -> String {
    let mut s = segs.join("/");
    if !q.is_empty() {
        s.push('?');
        s.push_str(&rewrite::encode_pairs(q));
    }
    s
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

/// Fetch one backend's views from upstream (no cache).
async fn fetch_views(up: &dyn MediaBackend) -> Result<Vec<Value>, String> {
    let server = up.name().to_string();
    let start = Instant::now();
    up.ensure_login().await.map_err(|e| e.to_string())?;
    let uc = up.session().await;
    let Some(uid) = uc.user_id.clone() else {
        return Err("no upstream user id".into());
    };
    let mut q = vec![];
    let segs = crate::paths::adapt(vec!["UserViews".into()], &mut q, uc.modern, Some(&uid));
    let r = up.get_json(&path_with_query(&segs, &q)).await;
    histogram!(names::CACHE_REFRESH_DURATION, "cache" => "views", "server" => server)
        .record(start.elapsed().as_secs_f64());
    match r {
        Ok((s, v)) if s.is_success() => Ok(v
            .get("Items")
            .and_then(|i| i.as_array())
            .cloned()
            .unwrap_or_default()),
        Ok((s, _)) => Err(format!("HTTP {s}")),
        Err(e) => Err(e.to_string()),
    }
}

/// Per-backend views through the catalog cache: fresh entries are used
/// as-is; otherwise the backend is asked, falling back to a stale entry
/// (`cache.serve_stale_on_error`) when it fails.
async fn raw_views(st: &Shared, force: bool) -> Vec<(ServerId, Vec<Value>)> {
    let _g = st.views_refresh.lock().await;
    let settings = st.catalog.settings();
    let set = st.backends();
    let futs = set.iter().map(|up| {
        let st = st.clone();
        async move {
            let key = CacheKey::new("views", up.name());
            let stale = match st.catalog.get(&key) {
                Lookup::Fresh(v) if !force => return items_of(up.id(), v),
                Lookup::Fresh(v) | Lookup::Stale(v) => Some(v),
                Lookup::Miss => None,
            };
            match fetch_views(up.as_ref()).await {
                Ok(items) => {
                    st.catalog.put(key, Value::Array(items.clone()));
                    Some((up.id(), items))
                }
                Err(e) => {
                    tracing::warn!(upstream = %up.name(), "views: {e}");
                    counter!(names::CACHE_REFRESH_FAILURES, "cache" => "views", "server" => up.name().to_string()).increment(1);
                    if settings.serve_stale_on_error {
                        stale.and_then(|v| items_of(up.id(), v))
                    } else {
                        None
                    }
                }
            }
        }
    });
    join_all(futs).await.into_iter().flatten().collect()
}

fn items_of(id: ServerId, v: Value) -> Option<(ServerId, Vec<Value>)> {
    match v {
        Value::Array(a) => Some((id, a)),
        _ => None,
    }
}

/// Refresh every backend's catalog entry now (background refresher).
pub async fn refresh_catalog(st: &Shared) {
    raw_views(st, true).await;
}

/// Build the merged, client-facing list of library views.
pub async fn views(st: &Shared, c: &Client, force: bool) -> Vec<Value> {
    let raw = raw_views(st, force).await;
    let _stage = Stage::start("views_build");
    let groups = merge::strategy_for(&st.cfg().merge).group_views(raw);
    let merged_n = groups
        .iter()
        .filter(|g| matches!(g, ViewGroup::Merged { .. }))
        .count();
    metrics::gauge!(names::LIBRARY_GROUPS, "kind" => "merged").set(merged_n as f64);
    metrics::gauge!(names::LIBRARY_GROUPS, "kind" => "single")
        .set((groups.len() - merged_n) as f64);
    let mut out = vec![];
    let mut name_count = std::collections::HashMap::<String, usize>::new();
    for g in &groups {
        let n = match g {
            ViewGroup::Single(_, v) => v.get("Name").and_then(|n| n.as_str()).unwrap_or(""),
            ViewGroup::Merged { name, .. } => name.as_str(),
        };
        *name_count.entry(n.to_lowercase()).or_default() += 1;
    }
    for g in groups {
        match g {
            ViewGroup::Single(u, mut v) => {
                let Some(up) = st.backend(u) else { continue };
                let uc = up.session().await;
                st.rewriters
                    .rewrite_json(&mut v, &out_ctx(st, up.as_ref(), &uc, c));
                if st.cfg().merge.label_colliding_libraries {
                    let name = v
                        .get("Name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    if name_count.get(&name.to_lowercase()).copied().unwrap_or(0) > 1 {
                        v["Name"] = json!(format!("{name} ({})", up.name()));
                    }
                }
                out.push(v);
            }
            ViewGroup::Merged {
                name,
                collection_type,
                members,
            } => {
                let id = synthetic_id(&[
                    "view",
                    &name.to_lowercase(),
                    &collection_type.clone().unwrap_or_default().to_lowercase(),
                ]);
                let (u0, first) = &members[0];
                let Some(up) = st.backend(*u0) else { continue };
                let uc = up.session().await;
                let mut t = first.clone();
                st.rewriters
                    .rewrite_json(&mut t, &out_ctx(st, up.as_ref(), &uc, c));
                t["Id"] = json!(id);
                t["DisplayPreferencesId"] = json!(id);
                t["Etag"] = json!(id);
                t["ParentId"] = json!(st.root_id);
                let member_ids = members
                    .iter()
                    .filter_map(|(u, v)| {
                        v.get("Id")
                            .and_then(|i| i.as_str())
                            .and_then(UpstreamItemId::parse)
                            .map(|i| (*u, i))
                    })
                    .collect();
                st.ids.register_view(MergedView {
                    id: id.clone(),
                    name,
                    collection_type,
                    members: member_ids,
                    template: t.clone(),
                });
                out.push(t);
            }
        }
    }
    out
}

/// Resolve a proxy id; on a miss, refresh views once (merged view ids are
/// not persisted) and retry.
async fn resolve_or_refresh(st: &Shared, id: &str) -> Option<Target> {
    if let Some(t) = st.ids.resolve(id) {
        return Some(t);
    }
    if !looks_like_id(id) {
        return None;
    }
    views(st, &anon_client(st), true).await;
    st.ids.resolve(id)
}

// ---------------------------------------------------------------------------
// Fan-out lists
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum Shape {
    QueryResult,
    Array,
    SearchHints,
}

/// How a fan-out list endpoint is queried and merged.
pub struct ListSpec {
    /// Metrics label.
    pub name: &'static str,
    /// Canonical (modern) upstream path.
    pub path: Vec<String>,
    pub shape: Shape,
    pub default_sort: Option<(&'static str, bool)>,
    pub dedupe_items: bool,
    pub dedupe_by_name: bool,
}

macro_rules! spec {
    ($name:ident, $label:expr, [$($seg:expr),*], $shape:expr, $sort:expr, $dd:expr, $dn:expr) => {
        pub static $name: std::sync::LazyLock<ListSpec> = std::sync::LazyLock::new(|| ListSpec {
            name: $label,
            path: vec![$($seg.to_string()),*],
            shape: $shape,
            default_sort: $sort,
            dedupe_items: $dd,
            dedupe_by_name: $dn,
        });
    };
}
spec!(
    ITEMS,
    "items",
    ["Items"],
    Shape::QueryResult,
    None,
    true,
    false
);
spec!(
    LATEST,
    "latest",
    ["Items", "Latest"],
    Shape::Array,
    Some(("DateCreated", true)),
    true,
    false
);
spec!(
    RESUME,
    "resume",
    ["UserItems", "Resume"],
    Shape::QueryResult,
    Some(("DatePlayed", true)),
    false,
    false
);
spec!(
    NEXTUP,
    "nextup",
    ["Shows", "NextUp"],
    Shape::QueryResult,
    None,
    false,
    false
);
spec!(
    SEARCH,
    "search",
    ["Search", "Hints"],
    Shape::SearchHints,
    None,
    true,
    false
);
#[allow(non_upper_case_globals)]
pub const NAMED: ListSpec = ListSpec {
    name: "named",
    path: Vec::new(),
    shape: Shape::QueryResult,
    default_sort: Some(("SortName", false)),
    dedupe_items: false,
    dedupe_by_name: true,
};

/// Upstreams (and the upstream-side parent id) a list request goes to.
async fn list_targets(
    st: &Shared,
    q: &[(String, String)],
) -> Vec<(ServerId, Option<UpstreamItemId>)> {
    let _s = Stage::start("id_decode");
    let parent = q_get(q, "ParentId")
        .filter(|p| normalize_hex_id(p).as_deref() != Some(st.root_id.as_str()));
    if let Some(p) = parent {
        return match resolve_or_refresh(st, p).await {
            Some(Target::View(v)) => v
                .members
                .iter()
                .map(|(u, id)| (*u, Some(id.clone())))
                .collect(),
            Some(Target::Item { upstream, id }) => vec![(upstream, Some(id))],
            None => {
                tracing::debug!("unknown ParentId {p}");
                vec![]
            }
        };
    }
    // Series / season scoped queries go to the owner only.
    for key in ["SeriesId", "SeasonId", "AlbumId"] {
        if let Some(v) = q_get(q, key) {
            if let Some(Target::Item { upstream, .. }) = resolve_or_refresh(st, v).await {
                return vec![(upstream, None)];
            }
        }
    }
    st.backends()
        .iter()
        .filter(|u| !u.is_down())
        .map(|u| (u.id(), None))
        .collect()
}

/// Translate the client query for one upstream. Returns None if the query
/// filters on ids none of which live on this upstream.
fn query_for(
    st: &Shared,
    q: &[(String, String)],
    u: ServerId,
    parent: Option<&str>,
    fanout: Option<(usize, Option<usize>)>,
) -> Option<Vec<(String, String)>> {
    let mut out = vec![];
    for (k, v) in q {
        let kl = k.to_ascii_lowercase();
        if is_token_key(k) || kl == "userid" {
            continue;
        }
        if kl == "parentid" {
            if let Some(p) = parent {
                out.push((k.clone(), p.to_string()));
            }
            continue;
        }
        if fanout.is_some() && (kl == "startindex" || kl == "limit") {
            continue;
        }
        let parts: Vec<&str> = v.split(',').collect();
        let mut kept = vec![];
        let mut had_ids = false;
        for part in parts {
            match normalize_hex_id(part).and_then(|n| st.ids.resolve(&n)) {
                Some(Target::Item { upstream, id }) => {
                    had_ids = true;
                    if upstream == u {
                        kept.push(id.into_string());
                    }
                }
                Some(Target::View(view)) => {
                    had_ids = true;
                    if let Some((_, id)) = view.members.iter().find(|(x, _)| *x == u) {
                        kept.push(id.to_string());
                    }
                }
                None => kept.push(part.to_string()),
            }
        }
        if had_ids && kept.is_empty() {
            return None;
        }
        out.push((k.clone(), kept.join(",")));
    }
    if let Some((start, limit)) = fanout {
        let want = match limit {
            Some(l) => (start + l).min(st.cfg().merge.fanout_limit),
            None => st.cfg().merge.fanout_limit,
        };
        out.push(("StartIndex".into(), "0".into()));
        out.push(("Limit".into(), want.to_string()));
    }
    Some(out)
}

/// Make sure the upstream `Fields` list contains `field` (case-insensitive),
/// adding the parameter if the client sent none.
pub(crate) fn ensure_field(q: &mut Vec<(String, String)>, field: &str) {
    if let Some((_, v)) = q.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case("fields")) {
        if !v.split(',').any(|f| f.trim().eq_ignore_ascii_case(field)) {
            if v.trim().is_empty() {
                *v = field.to_string();
            } else {
                v.push(',');
                v.push_str(field);
            }
        }
    } else {
        q.push(("Fields".into(), field.to_string()));
    }
}

fn fan_outcome(e: &BackendError) -> &'static str {
    if e.is_timeout() {
        "timeout"
    } else {
        "error"
    }
}

#[tracing::instrument(level = "debug", skip_all, fields(endpoint = spec.name))]
pub async fn fan_list(st: &Shared, r: &Req, c: &Client, spec: &ListSpec) -> Result<Response> {
    let cfg = st.cfg();
    let targets = list_targets(st, &r.query).await;
    let start: usize = q_get(&r.query, "StartIndex")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let limit: Option<usize> = q_get(&r.query, "Limit").and_then(|s| s.parse().ok());
    let fanout = targets.len() > 1;
    let endpoint = spec.name;
    let mut queried = 0usize;
    let mut futs = vec![];
    for (u, parent) in targets {
        let Some(up) = st.backend(u) else {
            continue;
        };
        let Some(q) = query_for(
            st,
            &r.query,
            u,
            parent.as_ref().map(|p| p.as_str()),
            fanout.then_some((start, limit)),
        ) else {
            // Filter ids live only on other upstreams: not an error.
            counter!(names::FANOUT_RESULTS, "endpoint" => endpoint, "server" => up.name().to_string(), "outcome" => "skipped").increment(1);
            continue;
        };
        let mut q = q;
        if fanout && spec.dedupe_items && cfg.merge.dedupe && spec.shape != Shape::SearchHints {
            // Real servers only return ProviderIds when asked; dedupe needs them.
            ensure_field(&mut q, "ProviderIds");
        }
        queried += 1;
        let path = spec.path.clone();
        futs.push(async move {
            let outcome = |o: &'static str| {
                counter!(names::FANOUT_RESULTS, "endpoint" => endpoint, "server" => up.name().to_string(), "outcome" => o).increment(1);
            };
            if let Err(e) = up.ensure_login().await {
                tracing::warn!(upstream = %up.name(), "skipping: {e}");
                outcome("skipped");
                return Err(());
            }
            let uc = up.session().await;
            let mut q = q;
            let segs = crate::paths::adapt(path, &mut q, uc.modern, uc.user_id.as_deref());
            match up.get_json(&path_with_query(&segs, &q)).await {
                Ok((s, v)) if s.is_success() => {
                    outcome("success");
                    Ok((up, uc, v))
                }
                Ok((s, _)) => {
                    tracing::warn!(upstream = %up.name(), endpoint, "list: HTTP {s}");
                    outcome("http_error");
                    Err(())
                }
                Err(e) => {
                    tracing::warn!(upstream = %up.name(), endpoint, "list: {e}");
                    outcome(fan_outcome(&e));
                    Err(())
                }
            }
        });
    }
    histogram!(names::FANOUT_UPSTREAMS, "endpoint" => endpoint).record(queried as f64);
    let fan_stage = Stage::start("fanout");
    let wait = Instant::now();
    let raw = join_all(futs).await;
    histogram!(names::FANOUT_SLOWEST, "endpoint" => endpoint).record(wait.elapsed().as_secs_f64());
    drop(fan_stage);
    let results: Vec<(Arc<dyn MediaBackend>, SessionInfo, Value)> =
        raw.into_iter().filter_map(|r| r.ok()).collect();
    if !results.is_empty() && results.len() < queried {
        counter!(names::FANOUT_PARTIAL, "endpoint" => endpoint).increment(1);
    }

    let rw_stage = Stage::start("rewrite");
    let mut all: Vec<Ranked> = vec![];
    let mut total = 0usize;
    for (up, uc, mut v) in results {
        let ctx = out_ctx(st, up.as_ref(), &uc, c);
        let (items, t) = match spec.shape {
            Shape::QueryResult => (
                v.get_mut("Items").map(|i| i.take()),
                v.get("TotalRecordCount").and_then(|t| t.as_u64()),
            ),
            Shape::SearchHints => (
                v.get_mut("SearchHints").map(|i| i.take()),
                v.get("TotalRecordCount").and_then(|t| t.as_u64()),
            ),
            Shape::Array => (Some(v), None),
        };
        let items = items.and_then(|i| match i {
            Value::Array(a) => Some(a),
            _ => None,
        });
        let mut items = items.unwrap_or_default();
        total += t.map(|t| t as usize).unwrap_or(items.len());
        for it in items.iter_mut() {
            st.rewriters.rewrite_json(it, &ctx);
        }
        let (priority, server) = (up.priority(), up.id());
        all.extend(items.into_iter().map(|item| Ranked {
            priority,
            server,
            item,
        }));
    }
    drop(rw_stage);
    counter!(names::MERGE_ITEMS_IN, "endpoint" => endpoint).increment(all.len() as u64);

    let mut removed = 0;
    let server_names = st.servers.names();
    if fanout && spec.dedupe_items && cfg.merge.dedupe {
        let _s = Stage::start("dedupe");
        let d = MeteredDeduplicator::new(
            merge::ProviderIdDeduplicator {
                types: cfg.merge.dedupe_types.clone(),
                // Search hints carry no ProviderIds: match on type+name+year.
                name_year_fallback: spec.shape == Shape::SearchHints,
            },
            &server_names,
        );
        let (a, stats) = d.dedupe(all);
        all = a;
        removed += stats.removed;
    }
    if fanout && spec.dedupe_by_name {
        let _s = Stage::start("dedupe");
        let d = MeteredDeduplicator::new(merge::NameDeduplicator, &server_names);
        let (a, stats) = d.dedupe(all);
        all = a;
        removed += stats.removed;
    }
    let mut items: Vec<Value> = all.into_iter().map(|r| r.item).collect();
    if fanout {
        let _s = Stage::start("sort_page");
        let mut sort = merge::parse_sort(q_get(&r.query, "SortBy"), q_get(&r.query, "SortOrder"));
        if sort.is_empty() {
            if let Some((f, d)) = spec.default_sort {
                sort = vec![(f.to_string(), d)];
            }
        }
        merge::sort_items(&mut items, &sort);
        let s = if spec.shape == Shape::Array { 0 } else { start };
        items = merge::page(items, s, limit);
    }
    counter!(names::MERGE_ITEMS_OUT, "endpoint" => endpoint).increment(items.len() as u64);
    let total = total.saturating_sub(removed);
    Ok(match spec.shape {
        Shape::QueryResult => ok_json(dto::query_result(items, total, start)),
        Shape::SearchHints => ok_json(json!({"SearchHints": items, "TotalRecordCount": total})),
        Shape::Array => ok_json(Value::Array(items)),
    })
}

/// Fan out a request returning an object and merge the objects:
/// arrays are unioned, numbers summed (`sum`) or the first value kept.
pub async fn fan_merge_objects(st: &Shared, r: &Req, c: &Client, sum: bool) -> Result<Response> {
    let targets = list_targets(st, &r.query).await;
    let segs: Vec<String> = r.segs.clone();
    let futs = targets.into_iter().filter_map(|(u, parent)| {
        let q = query_for(st, &r.query, u, parent.as_ref().map(|p| p.as_str()), None)?;
        let up = st.backend(u)?.clone();
        let segs = segs.clone();
        Some(async move {
            up.ensure_login().await.ok()?;
            let uc = up.session().await;
            let mut q = q;
            let segs = crate::paths::adapt(segs, &mut q, uc.modern, uc.user_id.as_deref());
            match up.get_json(&path_with_query(&segs, &q)).await {
                Ok((s, v)) if s.is_success() => Some((up, uc, v)),
                _ => None,
            }
        })
    });
    let mut acc = serde_json::Map::new();
    for (up, uc, mut v) in join_all(futs).await.into_iter().flatten() {
        st.rewriters
            .rewrite_json(&mut v, &out_ctx(st, up.as_ref(), &uc, c));
        let Value::Object(o) = v else { continue };
        for (k, val) in o {
            match (acc.get_mut(&k), val) {
                (None, val) => {
                    acc.insert(k, val);
                }
                (Some(Value::Array(a)), Value::Array(b)) => {
                    for x in b {
                        let key = x.get("Name").cloned().unwrap_or_else(|| x.clone());
                        if !a
                            .iter()
                            .any(|y| y.get("Name").cloned().unwrap_or_else(|| y.clone()) == key)
                        {
                            a.push(x);
                        }
                    }
                }
                (Some(Value::Number(a)), Value::Number(b)) if sum => {
                    *a = (a.as_i64().unwrap_or(0) + b.as_i64().unwrap_or(0)).into();
                }
                _ => {}
            }
        }
    }
    Ok(ok_json(Value::Object(acc)))
}

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
        if let Some(Target::Item { upstream, .. }) = resolve_or_refresh(st, &id).await {
            return forward(st, upstream, r, c, None).await;
        }
    }
    tracing::debug!("playstate report for unknown item, ignoring");
    Ok(no_content())
}

/// Find the owning upstream of a request by the first proxy id in its path,
/// then its query string.
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
    let pq = path_with_query(&segs, &q);
    // Never log `pq` itself at info: it may carry ids; tokens were stripped above.
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

/// Send a backend response to the client through the rewriter chain.
/// JSON and HLS playlists are buffered and rewritten; everything else is
/// streamed (only header rewriters run).
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

// ---------------------------------------------------------------------------
// WebSocket: minimal keep-alive so clients don't reconnect-loop.
// ---------------------------------------------------------------------------

async fn ws_handler(State(st): State<Shared>, req: Request) -> Response {
    let query: Vec<(String, String)> = req
        .uri()
        .query()
        .map(|q| {
            form_urlencoded::parse(q.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default();
    let info = auth::client_auth(req.headers(), &query);
    if info.get("token").and_then(|t| st.sessions.get(t)).is_none() {
        return err_resp(StatusCode::UNAUTHORIZED, "Unauthorized");
    }
    let (mut parts, _) = req.into_parts();
    use axum::extract::FromRequestParts;
    match WebSocketUpgrade::from_request_parts(&mut parts, &st).await {
        Ok(ws) => ws.on_upgrade(ws_loop),
        Err(e) => e.into_response(),
    }
}

async fn ws_loop(mut sock: WebSocket) {
    let hello = json!({"MessageType": "ForceKeepAlive", "Data": 60}).to_string();
    if sock.send(Message::Text(hello.into())).await.is_err() {
        return;
    }
    while let Some(Ok(msg)) = sock.recv().await {
        if let Message::Text(t) = msg {
            if t.contains("KeepAlive") {
                let ka = json!({"MessageType": "KeepAlive"}).to_string();
                if sock.send(Message::Text(ka.into())).await.is_err() {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Regression (real Jellyfin 12.3): ProviderIds are only returned when
    /// requested, so dedupe must ask for them whatever the client sent.
    #[test]
    fn ensure_field_adds_or_appends() {
        let mut a = q(&[("Limit", "5")]);
        ensure_field(&mut a, "ProviderIds");
        assert_eq!(a, q(&[("Limit", "5"), ("Fields", "ProviderIds")]));
        let mut b = q(&[("fields", "PrimaryImageAspectRatio,Path")]);
        ensure_field(&mut b, "ProviderIds");
        assert_eq!(
            b,
            q(&[("fields", "PrimaryImageAspectRatio,Path,ProviderIds")])
        );
        let mut c = q(&[("Fields", "providerids")]);
        ensure_field(&mut c, "ProviderIds");
        assert_eq!(c, q(&[("Fields", "providerids")]));
        let mut d = q(&[("Fields", "")]);
        ensure_field(&mut d, "ProviderIds");
        assert_eq!(d, q(&[("Fields", "ProviderIds")]));
    }
}
