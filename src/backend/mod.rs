//! Media backends (upstream servers).
//!
//! [`MediaBackend`] is the seam every upstream implements; the proxy core
//! only ever sees `Arc<dyn MediaBackend>` (a runtime list built from
//! `[[server]]`). Implementations:
//! * [`jellyfin::JellyfinBackend`]: a real Jellyfin server over HTTP(S);
//! * [`mock::MockBackend`]: in-memory, for tests and examples.
//!
//! A [`BackendFactory`] turns a `[[server]]` entry into a backend, so a new
//! kind of server (Emby, a static catalog, ...) is one new impl + factory.
//! How a backend authenticates is a separate seam,
//! [`credentials::UpstreamCredentials`]; HTTP itself goes through
//! [`transport::Transport`] (decorated with metrics by `Instrumented`).

pub mod accounts;
pub mod credentials;
pub mod jellyfin;
pub mod mock;
pub mod transport;

use crate::config::{Config, UpstreamConfig};
use crate::error::BackendError;
use crate::reload::Reloadable;
use crate::tls::TlsProvider;
use crate::types::ServerId;
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::pin::Pin;
use std::sync::Arc;

pub use jellyfin::JellyfinBackend;

/// Counts open requests/streams of a backend (see [`MediaBackend::in_flight`]).
#[derive(Debug, Default, Clone)]
pub struct InFlight(Arc<std::sync::atomic::AtomicUsize>);

impl InFlight {
    pub fn count(&self) -> usize {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
    /// Counted until the guard is dropped.
    pub fn guard(&self) -> InFlightGuard {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        InFlightGuard(self.0.clone())
    }
}

/// See [`InFlight::guard`].
#[derive(Debug)]
pub struct InFlightGuard(Arc<std::sync::atomic::AtomicUsize>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Keep `guard` alive until `body` is finished or dropped.
pub fn track_body(body: BodyStream, guard: InFlightGuard) -> BodyStream {
    Box::pin(body.map(move |chunk| {
        let _keep = &guard;
        chunk
    }))
}

/// Where a backend lives, split for URL rewriting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendLocation {
    /// Base URL without trailing slash, e.g. "http://h:8096/jellyfin".
    pub base: String,
    /// "http://h:8096"
    pub origin: String,
    /// "/jellyfin" or ""
    pub base_path: String,
}

impl BackendLocation {
    pub fn parse(server: &str, raw: &str) -> Result<Self, BackendError> {
        let url = url::Url::parse(raw).map_err(|e| BackendError::Request {
            server: server.to_string(),
            msg: format!("server url: {e}"),
        })?;
        Ok(BackendLocation {
            base: raw.trim_end_matches('/').to_string(),
            origin: url.origin().ascii_serialization(),
            base_path: url.path().trim_end_matches('/').to_string(),
        })
    }
    pub fn url(&self, path_and_query: &str) -> String {
        format!("{}/{}", self.base, path_and_query.trim_start_matches('/'))
    }
}

/// A request to a backend. Auth headers are added by the backend.
#[derive(Debug, Clone)]
pub struct BackendRequest {
    pub method: Method,
    /// Relative to the backend base, e.g. "Users/x/Items?ParentId=y".
    pub path_and_query: String,
    pub headers: HeaderMap,
    pub body: Option<Bytes>,
    /// Long-lived body (media, HLS): no total timeout, no redirects.
    pub streaming: bool,
    /// Proxy user id to act for: their `[[user.account]]` on this server
    /// if they have one, else the shared account.
    pub as_user: Option<String>,
}

impl BackendRequest {
    pub fn new(method: Method, path_and_query: impl Into<String>) -> Self {
        BackendRequest {
            method,
            path_and_query: path_and_query.into(),
            headers: HeaderMap::new(),
            body: None,
            streaming: false,
            as_user: None,
        }
    }
    pub fn get(path_and_query: impl Into<String>) -> Self {
        Self::new(Method::GET, path_and_query)
    }
    pub fn headers(mut self, h: HeaderMap) -> Self {
        self.headers = h;
        self
    }
    pub fn body(mut self, b: Option<Bytes>) -> Self {
        self.body = b;
        self
    }
    pub fn streaming(mut self, s: bool) -> Self {
        self.streaming = s;
        self
    }
    pub fn as_user(mut self, user: Option<&str>) -> Self {
        self.as_user = user.map(String::from);
        self
    }
}

pub type BodyStream = Pin<Box<dyn Stream<Item = Result<Bytes, BackendError>> + Send>>;

/// A backend response with a streaming body.
pub struct BackendResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: BodyStream,
}

impl std::fmt::Debug for BackendResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackendResponse")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl BackendResponse {
    pub fn from_bytes(status: StatusCode, headers: HeaderMap, body: Bytes) -> Self {
        BackendResponse {
            status,
            headers,
            body: Box::pin(futures_util::stream::once(async move { Ok(body) })),
        }
    }
    pub fn json(status: StatusCode, v: &Value) -> Self {
        let mut h = HeaderMap::new();
        h.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json; charset=utf-8"),
        );
        Self::from_bytes(
            status,
            h,
            Bytes::from(serde_json::to_vec(v).unwrap_or_default()),
        )
    }
    /// Collect the whole body.
    pub async fn bytes(mut self) -> Result<Bytes, BackendError> {
        let mut buf = bytes::BytesMut::new();
        while let Some(chunk) = self.body.next().await {
            buf.extend_from_slice(&chunk?);
        }
        Ok(buf.freeze())
    }
}

/// Per-backend session facts the proxy needs for rewriting.
#[derive(Debug, Clone, Default)]
pub struct SessionInfo {
    /// The upstream user the proxy acts as.
    pub user_id: Option<String>,
    /// Supports the user-less (10.9+) routes.
    pub modern: bool,
}

/// Health/status for the debug endpoint (no secrets).
#[derive(Debug, Clone, serde::Serialize)]
pub struct BackendStatus {
    pub name: String,
    pub kind: &'static str,
    pub url: String,
    pub priority: i64,
    pub healthy: bool,
    pub logged_in: bool,
    pub credentials: &'static str,
    pub version: Option<String>,
    pub user_id: Option<String>,
    pub token_age_secs: Option<u64>,
    pub modern_api: bool,
    /// Proxy users with their own account on this server.
    pub user_accounts: usize,
}

/// An upstream media server.
#[async_trait]
pub trait MediaBackend: Reloadable + Send + Sync {
    fn id(&self) -> ServerId;
    fn name(&self) -> &str;
    /// Implementation name, e.g. "jellyfin" or "mock".
    fn kind(&self) -> &'static str;
    /// Lower wins when de-duplicating.
    fn priority(&self) -> i64;
    fn location(&self) -> &BackendLocation;
    /// Temporarily unavailable (circuit open).
    fn is_down(&self) -> bool;
    async fn ensure_login(&self) -> Result<(), BackendError>;
    async fn session(&self) -> SessionInfo;
    async fn status(&self) -> BackendStatus;
    /// Refresh periodic gauges.
    async fn report_gauges(&self) {}
    /// Requests (including still-streaming response bodies) currently open.
    /// Used to drain a backend that was removed from the config.
    fn in_flight(&self) -> usize {
        0
    }
    /// The backend was removed (or replaced) and has drained: end the
    /// upstream session. Called once; the backend receives no new requests.
    async fn retire(&self) {}
    /// Send a request (authenticating as needed).
    async fn send(&self, req: BackendRequest) -> Result<BackendResponse, BackendError>;

    /// GET a JSON document. Non-2xx yields `(status, Null)`.
    async fn get_json(&self, path_and_query: &str) -> Result<(StatusCode, Value), BackendError> {
        self.get_json_as(None, path_and_query).await
    }

    /// Whether this proxy user has their own account on this server.
    fn has_account(&self, _user: &str) -> bool {
        false
    }

    /// Log in (the user's own account if they have one, else the shared
    /// one) and return the session to address the upstream with.
    async fn login_as(&self, user: Option<&str>) -> Result<SessionInfo, BackendError> {
        let _ = user;
        self.ensure_login().await?;
        Ok(self.session().await)
    }

    /// `get_json` acting for a proxy user (see [`BackendRequest::as_user`]).
    async fn get_json_as(
        &self,
        user: Option<&str>,
        path_and_query: &str,
    ) -> Result<(StatusCode, Value), BackendError> {
        let mut h = HeaderMap::new();
        h.insert("Accept", HeaderValue::from_static("application/json"));
        let r = self
            .send(BackendRequest::get(path_and_query).headers(h).as_user(user))
            .await?;
        let status = r.status;
        if !status.is_success() {
            return Ok((status, Value::Null));
        }
        let b = r.bytes().await?;
        Ok((status, serde_json::from_slice(&b).unwrap_or(Value::Null)))
    }
}

/// Creates backends from `[[server]]` entries.
pub trait BackendFactory: Send + Sync {
    fn create(
        &self,
        id: ServerId,
        cfg: &UpstreamConfig,
        global: &Config,
        tls: &dyn TlsProvider,
    ) -> Result<Arc<dyn MediaBackend>, BackendError>;
}

/// Default factory: every `[[server]]` is a Jellyfin server.
#[derive(Debug, Default, Clone, Copy)]
pub struct JellyfinFactory;

impl BackendFactory for JellyfinFactory {
    fn create(
        &self,
        id: ServerId,
        cfg: &UpstreamConfig,
        global: &Config,
        tls: &dyn TlsProvider,
    ) -> Result<Arc<dyn MediaBackend>, BackendError> {
        Ok(Arc::new(JellyfinBackend::new(
            id,
            cfg.clone(),
            global,
            tls,
        )?))
    }
}

/// Build the backend list for every enabled `[[server]]`, taking ids from
/// `registry`.
pub fn build_all(
    cfg: &Config,
    registry: &crate::servers::ServerRegistry,
    factory: &dyn BackendFactory,
    tls: &dyn TlsProvider,
) -> Result<Vec<Arc<dyn MediaBackend>>, BackendError> {
    cfg.upstreams
        .iter()
        .filter(|u| u.enabled)
        .map(|u| factory.create(registry.id_for(&u.name), u, cfg, tls))
        .collect()
}
