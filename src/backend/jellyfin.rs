//! [`MediaBackend`] for a real Jellyfin server.
//!
//! Login goes through an [`UpstreamCredentials`] (api key or password);
//! HTTP through a [`Transport`] (production: `Instrumented<HttpTransport>`).
//! Tokens are cached, refreshed once on a 401, and a server that fails to
//! connect is marked down for a short back-off.

use super::credentials::{self, AuthChannel, UpstreamCredentials};
use super::transport::{error_kind, HttpTransport, Instrumented, Transport};
use super::{
    track_body, BackendLocation, BackendRequest, BackendResponse, BackendStatus, InFlight,
    MediaBackend, SessionInfo,
};
use crate::config::{Config, Secret, UpstreamConfig, UpstreamTlsConfig};
use crate::error::{BackendError, ReloadError, TransportKind};
use crate::reload::{ReloadContext, Reloadable};
use crate::telemetry::names;
use crate::tls::TlsProvider;
use crate::types::ServerId;
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::TryStreamExt;
use metrics::{counter, gauge};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};

const CLIENT_NAME: &str = "Jellyfin Merge Proxy";
const DOWN_BACKOFF: Duration = Duration::from_secs(20);

#[derive(Default, Clone)]
struct AuthState {
    token: Option<Secret>,
    user_id: Option<String>,
    version: Option<String>,
    obtained: Option<Instant>,
}

impl std::fmt::Debug for AuthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthState")
            .field("token", &self.token.as_ref().map(|_| "***"))
            .field("user_id", &self.user_id)
            .field("version", &self.version)
            .finish()
    }
}

pub struct JellyfinBackend {
    id: ServerId,
    name: String,
    priority: i64,
    location: BackendLocation,
    cfg: std::sync::RwLock<UpstreamConfig>,
    tls: std::sync::RwLock<UpstreamTlsConfig>,
    timeout: std::sync::RwLock<Duration>,
    credentials: std::sync::RwLock<Arc<dyn UpstreamCredentials>>,
    transport: std::sync::RwLock<Arc<dyn Transport>>,
    state: RwLock<AuthState>,
    login_lock: Mutex<()>,
    down_until: std::sync::Mutex<Option<Instant>>,
    device_id: String,
    in_flight: InFlight,
    /// Set by `retire`: stragglers holding an old snapshot get `Down`
    /// instead of logging in again.
    retired: std::sync::atomic::AtomicBool,
}

impl std::fmt::Debug for JellyfinBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JellyfinBackend")
            .field("name", &self.name)
            .field("base", &self.location.base)
            .finish_non_exhaustive()
    }
}

fn timeout_of(cfg: &Config) -> Duration {
    Duration::from_secs(cfg.proxy.upstream_timeout_secs)
}

fn build_transport(
    name: &str,
    tls: &UpstreamTlsConfig,
    timeout: Duration,
    base_path: &str,
    provider: &dyn TlsProvider,
) -> Result<Arc<dyn Transport>, BackendError> {
    let rc = provider
        .upstream(tls, name)
        .map_err(|e| BackendError::Tls {
            server: name.to_string(),
            source: e,
        })?;
    let http = HttpTransport::build(timeout, rc).map_err(|e| BackendError::Request {
        server: name.to_string(),
        msg: format!("http client: {e}"),
    })?;
    Ok(Arc::new(Instrumented::new(http, name, base_path)))
}

fn record_client_cert_expiry(name: &str, tls: &UpstreamTlsConfig, provider: &dyn TlsProvider) {
    if let Some(p) = &tls.client_cert_path {
        if let Some(t) = provider.cert_expiry(p) {
            gauge!(names::TLS_CERT_EXPIRY, "cert" => format!("upstream:{name}")).set(t as f64);
        }
    }
}

fn transport_err(server: &str, e: reqwest::Error) -> BackendError {
    let kind = error_kind(&e);
    // Never put URLs (which may carry api_key) into errors/logs.
    let msg = e.without_url().to_string();
    BackendError::Transport {
        server: server.to_string(),
        kind,
        msg,
    }
}

impl JellyfinBackend {
    pub fn new(
        id: ServerId,
        cfg: UpstreamConfig,
        global: &Config,
        provider: &dyn TlsProvider,
    ) -> Result<Self, BackendError> {
        let location = BackendLocation::parse(&cfg.name, &cfg.url)?;
        let tls = UpstreamTlsConfig::merged(&global.upstream_tls, cfg.tls.as_ref());
        let timeout = timeout_of(global);
        let transport = build_transport(&cfg.name, &tls, timeout, &location.base_path, provider)?;
        record_client_cert_expiry(&cfg.name, &tls, provider);
        let device_id = crate::ids::hash_id(&["device", &cfg.name.to_ascii_lowercase()]);
        gauge!(names::UPSTREAM_UP, "server" => cfg.name.clone()).set(1.0);
        Ok(JellyfinBackend {
            id,
            name: cfg.name.clone(),
            priority: cfg.priority.unwrap_or(id.0 as i64),
            location,
            credentials: std::sync::RwLock::new(credentials::from_config(&cfg)),
            cfg: std::sync::RwLock::new(cfg),
            tls: std::sync::RwLock::new(tls),
            timeout: std::sync::RwLock::new(timeout),
            transport: std::sync::RwLock::new(transport),
            state: RwLock::new(AuthState::default()),
            login_lock: Mutex::new(()),
            down_until: std::sync::Mutex::new(None),
            device_id,
            in_flight: InFlight::default(),
            retired: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Replace the (decorated) transport, e.g. to wrap it in another decorator.
    pub fn set_transport(&self, t: Arc<dyn Transport>) {
        *self.transport.write().unwrap() = t;
    }

    fn auth_header(&self, token: Option<&str>) -> String {
        let mut s = format!(
            "MediaBrowser Client=\"{CLIENT_NAME}\", Device=\"jellyfin-merge-proxy\", DeviceId=\"{}\", Version=\"{}\"",
            self.device_id,
            env!("CARGO_PKG_VERSION")
        );
        if let Some(t) = token {
            s.push_str(&format!(", Token=\"{t}\""));
        }
        s
    }

    fn auth_headers(&self, token: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Ok(hv) = HeaderValue::from_str(&self.auth_header(token)) {
            h.insert(AUTHORIZATION, hv.clone());
            h.insert("X-Emby-Authorization", hv);
        }
        if let Some(t) = token {
            if let Ok(mut hv) = HeaderValue::from_str(t) {
                hv.set_sensitive(true);
                h.insert("X-Emby-Token", hv);
            }
        }
        h
    }

    fn mark_down(&self, why: &str) {
        tracing::warn!(upstream = %self.name, "upstream unavailable: {why}");
        *self.down_until.lock().unwrap() = Some(Instant::now() + DOWN_BACKOFF);
        gauge!(names::UPSTREAM_UP, "server" => self.name.clone()).set(0.0);
    }

    fn mark_up(&self) {
        *self.down_until.lock().unwrap() = None;
        gauge!(names::UPSTREAM_UP, "server" => self.name.clone()).set(1.0);
    }

    async fn token(&self) -> Option<String> {
        self.state
            .read()
            .await
            .token
            .as_ref()
            .map(|t| t.expose().to_string())
    }

    fn build_request(
        &self,
        method: Method,
        pq: &str,
        headers: HeaderMap,
        body: Option<Bytes>,
    ) -> Result<reqwest::Request, BackendError> {
        let url =
            reqwest::Url::parse(&self.location.url(pq)).map_err(|e| BackendError::Request {
                server: self.name.clone(),
                msg: e.to_string(),
            })?;
        let mut r = reqwest::Request::new(method, url);
        *r.headers_mut() = headers;
        if let Some(b) = body {
            *r.body_mut() = Some(reqwest::Body::from(b));
        }
        Ok(r)
    }

    async fn raw_send(
        &self,
        req: reqwest::Request,
        streaming: bool,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let t = self.transport.read().unwrap().clone();
        t.send(req, streaming).await
    }

    /// Log in. `stale` is the token that just failed, to avoid re-login storms.
    #[tracing::instrument(level = "debug", skip_all, fields(upstream = %self.name))]
    async fn login(&self, stale: Option<&str>) -> Result<(), BackendError> {
        let _g = self.login_lock.lock().await;
        {
            let st = self.state.read().await;
            if let Some(t) = &st.token {
                if Some(t.expose()) != stale {
                    return Ok(());
                }
            }
        }
        let r = self.do_login().await;
        let outcome = match &r {
            Ok(()) => "success",
            Err(BackendError::Unreachable { .. }) => "unreachable",
            Err(_) => "failure",
        };
        counter!(names::UPSTREAM_AUTH, "server" => self.name.clone(), "outcome" => outcome)
            .increment(1);
        r
    }

    async fn do_login(&self) -> Result<(), BackendError> {
        let mut new = AuthState::default();
        let (_, info) = self
            .call(Method::GET, "System/Info/Public", None, None)
            .await?;
        new.version = info
            .get("Version")
            .and_then(|v| v.as_str())
            .map(String::from);
        let creds = self.credentials.read().unwrap().clone();
        let grant = creds.login(self).await?;
        new.token = Some(grant.token);
        new.user_id = grant.user_id;
        new.obtained = Some(Instant::now());
        tracing::info!(upstream = %self.name, version = ?new.version, user_id = ?new.user_id, credentials = creds.kind(), "logged in to upstream");
        self.mark_up();
        gauge!(names::UPSTREAM_TOKEN_AGE, "server" => self.name.clone()).set(0.0);
        *self.state.write().await = new;
        Ok(())
    }
}

#[async_trait]
impl AuthChannel for JellyfinBackend {
    fn server_name(&self) -> &str {
        &self.name
    }
    async fn call(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> Result<(StatusCode, Value), BackendError> {
        let mut h = if token.is_some() || body.is_some() {
            self.auth_headers(token)
        } else {
            HeaderMap::new()
        };
        let body = body.map(|b| {
            h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            Bytes::from(serde_json::to_vec(&b).unwrap_or_default())
        });
        let req = self.build_request(method, path, h, body)?;
        let resp = self.raw_send(req, false).await.map_err(|e| {
            let e = e.without_url();
            self.mark_down(&e.to_string());
            BackendError::Unreachable {
                server: self.name.clone(),
                msg: e.to_string(),
            }
        })?;
        let status = resp.status();
        let v = resp.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, v))
    }
}

#[async_trait]
impl MediaBackend for JellyfinBackend {
    fn id(&self) -> ServerId {
        self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn kind(&self) -> &'static str {
        "jellyfin"
    }
    fn priority(&self) -> i64 {
        self.priority
    }
    fn location(&self) -> &BackendLocation {
        &self.location
    }
    fn is_down(&self) -> bool {
        matches!(*self.down_until.lock().unwrap(), Some(t) if t > Instant::now())
    }

    async fn ensure_login(&self) -> Result<(), BackendError> {
        if self.state.read().await.token.is_some() {
            return Ok(());
        }
        self.login(None).await
    }

    async fn session(&self) -> SessionInfo {
        let st = self.state.read().await;
        SessionInfo {
            user_id: st.user_id.clone(),
            modern: st
                .version
                .as_deref()
                .map(|v| version_at_least(v, 10, 9))
                .unwrap_or(true),
        }
    }

    async fn status(&self) -> BackendStatus {
        let st = self.state.read().await.clone();
        BackendStatus {
            name: self.name.clone(),
            kind: self.kind(),
            url: self.location.base.clone(),
            priority: self.priority,
            healthy: !self.is_down(),
            logged_in: st.token.is_some(),
            credentials: self.credentials.read().unwrap().kind(),
            modern_api: st
                .version
                .as_deref()
                .map(|v| version_at_least(v, 10, 9))
                .unwrap_or(true),
            version: st.version,
            user_id: st.user_id,
            token_age_secs: st.obtained.map(|t| t.elapsed().as_secs()),
        }
    }

    async fn report_gauges(&self) {
        let st = self.state.read().await;
        if let Some(t) = st.obtained {
            gauge!(names::UPSTREAM_TOKEN_AGE, "server" => self.name.clone())
                .set(t.elapsed().as_secs_f64());
        }
        gauge!(names::UPSTREAM_UP, "server" => self.name.clone()).set(if self.is_down() {
            0.0
        } else {
            1.0
        });
    }

    fn in_flight(&self) -> usize {
        self.in_flight.count()
    }

    /// Ends a password login's upstream session (`POST Sessions/Logout`).
    /// API keys are left alone: logging out with one would revoke the key.
    async fn retire(&self) {
        self.retired
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let token = self.state.read().await.token.clone();
        let is_password = self.credentials.read().unwrap().kind() == "password";
        *self.state.write().await = AuthState::default();
        let Some(token) = token.filter(|_| is_password) else {
            return;
        };
        let r = self.build_request(
            Method::POST,
            "Sessions/Logout",
            self.auth_headers(Some(token.expose())),
            None,
        );
        match r {
            Ok(r) => match self.raw_send(r, false).await {
                Ok(resp) => {
                    tracing::info!(upstream = %self.name, status = resp.status().as_u16(), "ended upstream session")
                }
                Err(e) => {
                    tracing::debug!(upstream = %self.name, "logout failed: {}", e.without_url())
                }
            },
            Err(e) => tracing::debug!(upstream = %self.name, "logout: {e}"),
        }
    }

    /// Retries once after re-login on 401.
    #[tracing::instrument(level = "debug", skip_all, fields(upstream = %self.name, method = %req.method, streaming = req.streaming))]
    async fn send(&self, req: BackendRequest) -> Result<BackendResponse, BackendError> {
        // Counted until the response body is finished (see `retire`).
        let guard = self.in_flight.guard();
        if self.is_down() || self.retired.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(BackendError::Down {
                server: self.name.clone(),
            });
        }
        self.ensure_login().await?;
        let mut attempt = 0;
        loop {
            let token = self.token().await;
            let mut h = req.headers.clone();
            h.extend(self.auth_headers(token.as_deref()));
            let r =
                self.build_request(req.method.clone(), &req.path_and_query, h, req.body.clone())?;
            let resp = match self.raw_send(r, req.streaming).await {
                Ok(r) => r,
                Err(e) => {
                    if e.is_connect() {
                        self.mark_down(&e.without_url().to_string());
                        return Err(BackendError::Transport {
                            server: self.name.clone(),
                            kind: TransportKind::Connect,
                            msg: "connection failed".into(),
                        });
                    }
                    return Err(transport_err(&self.name, e));
                }
            };
            if resp.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                attempt += 1;
                tracing::info!(upstream = %self.name, "401 from upstream, re-authenticating");
                counter!(names::UPSTREAM_REAUTH, "server" => self.name.clone()).increment(1);
                counter!(names::UPSTREAM_RETRIES, "server" => self.name.clone(), "reason" => "unauthorized").increment(1);
                self.login(token.as_deref()).await?;
                continue;
            }
            let status = resp.status();
            let headers = resp.headers().clone();
            let server = self.name.clone();
            let body = resp
                .bytes_stream()
                .map_err(move |e| transport_err(&server, e));
            return Ok(BackendResponse {
                status,
                headers,
                body: track_body(Box::pin(body), guard),
            });
        }
    }
}

#[async_trait]
impl Reloadable for JellyfinBackend {
    fn reload_name(&self) -> String {
        format!("backend:{}", self.name)
    }

    fn validate(&self, cx: &ReloadContext<'_>) -> Result<(), ReloadError> {
        let Some(nc) = own_config(cx.new, &self.name) else {
            return Ok(());
        };
        let tls = UpstreamTlsConfig::merged(&cx.new.upstream_tls, nc.tls.as_ref());
        cx.tls
            .upstream(&tls, &self.name)
            .map_err(|e| ReloadError::Tls {
                what: format!("[[server]] '{}' TLS", self.name),
                source: e,
            })?;
        Ok(())
    }

    /// Rebuilds the HTTP clients if TLS/timeout changed (or TLS files
    /// changed on disk); drops the cached token if the credentials changed.
    async fn apply(&self, cx: &ReloadContext<'_>) -> Result<bool, ReloadError> {
        let Some(nc) = own_config(cx.new, &self.name) else {
            return Ok(false);
        };
        let oc = own_config(cx.old, &self.name);
        let tls = UpstreamTlsConfig::merged(&cx.new.upstream_tls, nc.tls.as_ref());
        let timeout = timeout_of(cx.new);
        let old = self.cfg.read().unwrap().clone();
        let tls_changed =
            tls != *self.tls.read().unwrap() || timeout != *self.timeout.read().unwrap();
        if oc == Some(nc) && !tls_changed && !cx.files_changed {
            return Ok(false);
        }
        if oc != Some(nc) {
            counter!(names::CONFIG_UPSTREAM_CHANGES, "change" => "changed").increment(1);
        }
        let t = match build_transport(&self.name, &tls, timeout, &self.location.base_path, cx.tls) {
            Ok(t) => {
                counter!(names::TLS_RELOADS, "target" => "upstream", "outcome" => "success")
                    .increment(1);
                t
            }
            Err(e) => {
                counter!(names::TLS_RELOADS, "target" => "upstream", "outcome" => "error")
                    .increment(1);
                return Err(e.into());
            }
        };
        self.set_transport(t);
        record_client_cert_expiry(&self.name, &tls, cx.tls);
        *self.tls.write().unwrap() = tls;
        *self.timeout.write().unwrap() = timeout;
        let creds_changed = old.api_key != nc.api_key
            || old.username != nc.username
            || old.password != nc.password
            || old.user_id != nc.user_id;
        *self.cfg.write().unwrap() = nc.clone();
        if creds_changed {
            tracing::info!(upstream = %self.name, "credentials changed; will re-login");
            *self.credentials.write().unwrap() = credentials::from_config(nc);
            *self.state.write().await = AuthState::default();
            self.mark_up();
        }
        Ok(true)
    }
}

fn own_config<'a>(cfg: &'a Config, name: &str) -> Option<&'a UpstreamConfig> {
    cfg.upstreams
        .iter()
        .find(|u| u.enabled && u.name.eq_ignore_ascii_case(name))
}

pub fn version_at_least(v: &str, major: u64, minor: u64) -> bool {
    let mut it = v.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    let ma = it.next().unwrap_or(0);
    let mi = it.next().unwrap_or(0);
    (ma, mi) >= (major, minor)
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions() {
        assert!(super::version_at_least("10.9.0", 10, 9));
        assert!(super::version_at_least("10.10.7", 10, 9));
        assert!(!super::version_at_least("10.8.13", 10, 9));
    }
}
