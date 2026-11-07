//! An in-memory [`MediaBackend`] for tests and examples.
//!
//! Routes are matched on the path (case-insensitive, query ignored); every
//! request is recorded. Plug it in with [`MockFactory`] through
//! `StateOptions::backend_factory`.

use super::{
    track_body, BackendFactory, BackendLocation, BackendRequest, BackendResponse, BackendStatus,
    InFlight, InFlightGuard, MediaBackend, SessionInfo,
};
use crate::config::{Config, UpstreamConfig};
use crate::error::{BackendError, ReloadError};
use crate::reload::{ReloadContext, Reloadable};
use crate::tls::TlsProvider;
use crate::types::ServerId;
use async_trait::async_trait;
use bytes::Bytes;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Builds a JSON response from the request's query string.
pub type MockHandler = Arc<dyn Fn(&[(String, String)]) -> Value + Send + Sync>;

#[derive(Clone)]
struct MockRoute {
    method: Option<Method>,
    path: String,
    status: StatusCode,
    content_type: String,
    body: Bytes,
    handler: Option<MockHandler>,
}

impl std::fmt::Debug for MockRoute {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockRoute")
            .field("method", &self.method)
            .field("path", &self.path)
            .field("dynamic", &self.handler.is_some())
            .finish()
    }
}

fn query_pairs(path_and_query: &str) -> Vec<(String, String)> {
    let q = path_and_query.split_once('?').map(|(_, q)| q).unwrap_or("");
    url::form_urlencoded::parse(q.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

/// A recorded request.
#[derive(Debug, Clone)]
pub struct MockCall {
    pub method: Method,
    pub path_and_query: String,
}

#[derive(Debug)]
pub struct MockBackend {
    id: ServerId,
    name: String,
    priority: i64,
    location: BackendLocation,
    user_id: String,
    version: Mutex<Option<String>>,
    routes: Mutex<Vec<MockRoute>>,
    calls: Mutex<Vec<MockCall>>,
    down: AtomicBool,
    retired: AtomicBool,
    in_flight: InFlight,
}

fn norm(p: &str) -> String {
    p.split('?')
        .next()
        .unwrap_or("")
        .trim_matches('/')
        .to_ascii_lowercase()
}

impl MockBackend {
    pub fn new(id: ServerId, name: &str, priority: i64) -> Self {
        let base = format!("http://mock-{}.invalid", name.to_ascii_lowercase());
        let m = MockBackend {
            id,
            name: name.to_string(),
            priority,
            location: BackendLocation {
                origin: base.clone(),
                base,
                base_path: String::new(),
            },
            user_id: crate::ids::hash_id(&["mock-user", name]),
            version: Mutex::new(Some("10.10.7".into())),
            routes: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            down: AtomicBool::new(false),
            retired: AtomicBool::new(false),
            in_flight: InFlight::default(),
        };
        m.on_json(
            Some(Method::GET),
            "System/Info/Public",
            json!({"Version": "10.10.7", "ServerName": name, "Id": crate::ids::hash_id(&["mock", name])}),
        );
        m
    }

    /// The upstream user id this mock pretends to log in as.
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// Respond to `path` with JSON (later routes win).
    pub fn on_json(&self, method: Option<Method>, path: &str, body: Value) -> &Self {
        self.on(
            method,
            path,
            StatusCode::OK,
            "application/json; charset=utf-8",
            Bytes::from(serde_json::to_vec(&body).unwrap_or_default()),
        )
    }

    pub fn on(
        &self,
        method: Option<Method>,
        path: &str,
        status: StatusCode,
        content_type: &str,
        body: Bytes,
    ) -> &Self {
        self.routes.lock().unwrap().push(MockRoute {
            method,
            path: norm(path),
            status,
            content_type: content_type.to_string(),
            body,
            handler: None,
        });
        self
    }

    /// Respond to `path` with JSON computed from the query (a fake server
    /// that honours `Ids`, `StartIndex`, ... ).
    pub fn on_query(
        &self,
        method: Option<Method>,
        path: &str,
        f: impl Fn(&[(String, String)]) -> Value + Send + Sync + 'static,
    ) -> &Self {
        self.routes.lock().unwrap().push(MockRoute {
            method,
            path: norm(path),
            status: StatusCode::OK,
            content_type: "application/json; charset=utf-8".into(),
            body: Bytes::new(),
            handler: Some(Arc::new(f)),
        });
        self
    }

    pub fn calls(&self) -> Vec<MockCall> {
        self.calls.lock().unwrap().clone()
    }

    /// Pretend to live at `url` (what `[[server]] server = ...` says), so a
    /// reload can tell a URL change from an unchanged server.
    pub fn at_url(mut self, url: &str) -> Self {
        if let Ok(l) = BackendLocation::parse(&self.name, url) {
            self.location = l;
        }
        self
    }

    /// `retire` was called (the server was removed/rebuilt and drained).
    pub fn is_retired(&self) -> bool {
        self.retired.load(Ordering::Relaxed)
    }

    /// Simulate an open request/stream until the guard is dropped.
    pub fn hold(&self) -> InFlightGuard {
        self.in_flight.guard()
    }

    pub fn set_down(&self, down: bool) {
        self.down.store(down, Ordering::Relaxed);
    }

    /// Version reported via [`MediaBackend::status`] (and used when the
    /// proxy auto-picks `reported_version`).
    pub fn set_version(&self, version: Option<&str>) {
        *self.version.lock().unwrap() = version.map(str::to_string);
    }
}

#[async_trait]
impl Reloadable for MockBackend {
    fn reload_name(&self) -> String {
        format!("backend:{}", self.name)
    }
    async fn apply(&self, _cx: &ReloadContext<'_>) -> Result<bool, ReloadError> {
        Ok(false)
    }
}

#[async_trait]
impl MediaBackend for MockBackend {
    fn id(&self) -> ServerId {
        self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn kind(&self) -> &'static str {
        "mock"
    }
    fn priority(&self) -> i64 {
        self.priority
    }
    fn location(&self) -> &BackendLocation {
        &self.location
    }
    fn is_down(&self) -> bool {
        self.down.load(Ordering::Relaxed)
    }
    async fn ensure_login(&self) -> Result<(), BackendError> {
        if self.is_down() {
            return Err(BackendError::Down {
                server: self.name.clone(),
            });
        }
        Ok(())
    }
    async fn session(&self) -> SessionInfo {
        SessionInfo {
            user_id: Some(self.user_id.clone()),
            modern: true,
        }
    }
    async fn status(&self) -> BackendStatus {
        BackendStatus {
            name: self.name.clone(),
            kind: self.kind(),
            url: self.location.base.clone(),
            priority: self.priority,
            healthy: !self.is_down(),
            logged_in: true,
            credentials: "none",
            version: self.version.lock().unwrap().clone(),
            user_id: Some(self.user_id.clone()),
            token_age_secs: None,
            user_accounts: 0,
            modern_api: true,
        }
    }
    fn in_flight(&self) -> usize {
        self.in_flight.count()
    }
    async fn retire(&self) {
        self.retired.store(true, Ordering::Relaxed);
    }
    async fn send(&self, req: BackendRequest) -> Result<BackendResponse, BackendError> {
        self.ensure_login().await?;
        let guard = self.in_flight.guard();
        self.calls.lock().unwrap().push(MockCall {
            method: req.method.clone(),
            path_and_query: req.path_and_query.clone(),
        });
        let p = norm(&req.path_and_query);
        let routes = self.routes.lock().unwrap();
        let hit = routes
            .iter()
            .rev()
            .find(|r| r.path == p && r.method.as_ref().is_none_or(|m| *m == req.method));
        let r = match hit {
            Some(r) => {
                let mut h = HeaderMap::new();
                if let Ok(v) = HeaderValue::from_str(&r.content_type) {
                    h.insert(CONTENT_TYPE, v);
                }
                let body = match &r.handler {
                    Some(f) => Bytes::from(
                        serde_json::to_vec(&f(&query_pairs(&req.path_and_query)))
                            .unwrap_or_default(),
                    ),
                    None => r.body.clone(),
                };
                BackendResponse::from_bytes(r.status, h, body)
            }
            None => {
                BackendResponse::from_bytes(StatusCode::NOT_FOUND, HeaderMap::new(), Bytes::new())
            }
        };
        Ok(BackendResponse {
            body: track_body(r.body, guard),
            ..r
        })
    }
}

type MockSetup = dyn Fn(&MockBackend, &UpstreamConfig) + Send + Sync;

/// Creates a [`MockBackend`] per `[[server]]` and keeps them for inspection.
pub struct MockFactory {
    setup: Box<MockSetup>,
    created: Mutex<Vec<Arc<MockBackend>>>,
}

impl MockFactory {
    /// `setup` installs routes on each new mock.
    pub fn new(setup: impl Fn(&MockBackend, &UpstreamConfig) + Send + Sync + 'static) -> Self {
        MockFactory {
            setup: Box::new(setup),
            created: Mutex::new(Vec::new()),
        }
    }
    pub fn created(&self) -> Vec<Arc<MockBackend>> {
        self.created.lock().unwrap().clone()
    }
}

impl BackendFactory for MockFactory {
    fn create(
        &self,
        id: ServerId,
        cfg: &UpstreamConfig,
        _global: &Config,
        _tls: &dyn TlsProvider,
    ) -> Result<Arc<dyn MediaBackend>, BackendError> {
        let m = Arc::new(
            MockBackend::new(id, &cfg.name, cfg.priority.unwrap_or(id.0 as i64)).at_url(&cfg.url),
        );
        (self.setup)(&m, cfg);
        self.created.lock().unwrap().push(m.clone());
        Ok(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn routes_and_records() {
        let m = MockBackend::new(ServerId(0), "a", 0);
        m.on_json(None, "/Users/x/Views", json!({"Items": []}));
        let (s, v) = m.get_json("Users/x/Views?foo=1").await.unwrap();
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v["Items"], json!([]));
        let (s, _) = m.get_json("Nope").await.unwrap();
        assert_eq!(s, StatusCode::NOT_FOUND);
        assert_eq!(m.calls().len(), 2);
        m.set_down(true);
        assert!(m.get_json("Users/x/Views").await.is_err());
    }
}
