//! [`MediaBackend`] for a real Jellyfin server.
//!
//! Login goes through an [`UpstreamCredentials`] (api key or password);
//! HTTP through a [`Transport`] (production: `Instrumented<HttpTransport>`).
//! Tokens are cached, refreshed once on a 401, and a server that fails to
//! connect is marked down for a short back-off.

use super::accounts::AccountPool;
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
    /// Reloadable in place (`[[server]] priority`).
    priority: std::sync::atomic::AtomicI64,
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
    /// Per-user accounts on this server (`[[user.account]]`).
    accounts: AccountPool,
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
        let accounts = AccountPool::from_config(&cfg.name, global);
        Ok(JellyfinBackend {
            id,
            name: cfg.name.clone(),
            priority: std::sync::atomic::AtomicI64::new(cfg.priority.unwrap_or(id.0 as i64)),
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
            accounts,
        })
    }

    /// Replace the (decorated) transport, e.g. to wrap it in another decorator.
    pub fn set_transport(&self, t: Arc<dyn Transport>) {
        *self.transport.write().unwrap() = t;
    }

    fn auth_header(&self, token: Option<&str>, device: &str) -> String {
        let mut s = format!(
            "MediaBrowser Client=\"{CLIENT_NAME}\", Device=\"jellyfin-merge-proxy\", DeviceId=\"{}\", Version=\"{}\"",
            device,
            env!("CARGO_PKG_VERSION")
        );
        if let Some(t) = token {
            s.push_str(&format!(", Token=\"{t}\""));
        }
        s
    }

    /// Per-user accounts use their own device id: Jellyfin ends a user's
    /// older sessions on the same device when it logs in again.
    fn auth_headers_for(&self, token: Option<&str>, device: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Ok(hv) = HeaderValue::from_str(&self.auth_header(token, device)) {
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

    /// End an upstream session (`POST Sessions/Logout`). Never for API
    /// keys: logging out with one would revoke the key.
    async fn logout(&self, token: &Secret, device: &str) {
        let r = self.build_request(
            Method::POST,
            "Sessions/Logout",
            self.auth_headers_for(Some(token.expose()), device),
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

    /// Log out password sessions of per-user accounts that were removed,
    /// changed, or are being retired.
    async fn logout_accounts(&self, list: Vec<Arc<super::accounts::Account>>) {
        for a in list {
            let is_password = a.kind() == "password";
            if let Some(l) = a.take().await.filter(|_| is_password) {
                self.logout(&l.token, &a.device_id).await;
            }
        }
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
        // Keep Version even if creds.login fails next — status() and blank
        // reported_version need it before a successful session exists.
        if let Some(v) = new.version.clone() {
            self.state.write().await.version = Some(v);
        }
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

pub fn version_at_least(v: &str, major: u64, minor: u64) -> bool {
    let mut it = v.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    let ma = it.next().unwrap_or(0);
    let mi = it.next().unwrap_or(0);
    (ma, mi) >= (major, minor)
}

/// Compare dotted numeric versions (`10.11.0` > `10.10.7`). Non-numeric
/// segments count as 0; missing trailing segments are 0.
pub fn cmp_version(a: &str, b: &str) -> std::cmp::Ordering {
    let mut aa = a.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    let mut bb = b.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
    loop {
        match (aa.next(), bb.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
            }
            (Some(x), None) => {
                if x != 0 {
                    return std::cmp::Ordering::Greater;
                }
            }
            (None, Some(y)) => {
                if y != 0 {
                    return std::cmp::Ordering::Less;
                }
            }
        }
    }
}

mod channel;
mod media;
mod reloadable;

#[cfg(test)]
mod tests;
