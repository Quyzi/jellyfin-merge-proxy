#![recursion_limit = "512"]
//! jellyfin-merge-proxy: present several Jellyfin servers as one.
//!
//! See `ARCHITECTURE.md` for the trait seams. In short, [`AppState`] holds
//! only trait objects / generic decorators:
//! * backends: an `ArcSwap` of [`servers::BackendSet`] (`Arc<dyn backend::MediaBackend>`s
//!   from a [`backend::BackendFactory`]);
//! * ids: `Arc<dyn ids::IdMapper>`; users: `Arc<dyn users::ProxyAuthenticator>`;
//! * catalog: `Arc<dyn cache::CatalogCache>`; TLS: `Arc<dyn tls::TlsProvider>`;
//! * HTTP: an [`endpoints::Registry`] and a [`rewrite::RewriteChain`].

pub mod auth;
pub mod backend;
pub mod cache;
pub mod config;
pub mod dto;
pub mod endpoints;
pub mod error;
pub mod ids;
pub mod merge;
pub mod password;
pub mod paths;
pub mod proxy;
pub mod reload;
pub mod rewrite;
pub mod server;
pub mod servers;
pub mod telemetry;
pub mod tls;
pub mod types;
pub mod users;

/// Backwards-compatible path for the old `upstream` module.
pub mod upstream {
    pub use crate::backend::jellyfin::version_at_least;
    pub use crate::backend::transport::{error_kind, HttpTransport, Instrumented, Transport};
    pub use crate::backend::{JellyfinBackend as Upstream, MediaBackend};
}

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use arc_swap::ArcSwap;
use backend::{BackendFactory, JellyfinFactory, MediaBackend};
use cache::CatalogCache;
use config::{Config, PasswordCache};
use ids::IdMapper;
use metrics::gauge;
use serde_json::Value;
use servers::{BackendSet, ServerRegistry, ServerState};
use telemetry::names;
use tls::{RustlsProvider, TlsProvider};
use types::ServerId;
use users::ProxyAuthenticator;

pub struct AppState {
    cfg: RwLock<Arc<Config>>,
    pub server_id: String,
    pub root_id: String,
    /// The enabled `[[server]]`s; use [`AppState::backends`] to take a
    /// snapshot for one request.
    backends: Arc<ArcSwap<BackendSet>>,
    /// Every server name ever configured in this process, by [`ServerId`].
    pub servers: Arc<ServerRegistry>,
    pub backend_factory: Arc<dyn BackendFactory>,
    pub ids: Arc<dyn IdMapper>,
    pub sessions: Arc<auth::Sessions>,
    pub authenticator: Arc<dyn ProxyAuthenticator>,
    pub catalog: Arc<dyn CatalogCache>,
    /// Serialises catalog refreshes.
    pub views_refresh: tokio::sync::Mutex<()>,
    pub rewriters: rewrite::RewriteChain,
    pub endpoints: endpoints::Registry,
    pub tls_provider: Arc<dyn TlsProvider>,
    /// In-memory display preferences keyed by (user, id, client).
    pub display_prefs: Mutex<HashMap<String, Value>>,
    pub started: Instant,
    password_cache: PasswordCache,
}

pub type Shared = Arc<AppState>;

/// Optional overrides for every seam (all default to the production impls).
#[derive(Default)]
pub struct StateOptions {
    pub password_cache: Option<PasswordCache>,
    pub backend_factory: Option<Arc<dyn BackendFactory>>,
    pub tls_provider: Option<Arc<dyn TlsProvider>>,
    pub catalog: Option<Arc<dyn CatalogCache>>,
    pub endpoints: Option<endpoints::Registry>,
    pub rewriters: Option<rewrite::RewriteChain>,
}

impl AppState {
    pub fn new(cfg: Config) -> anyhow::Result<Shared> {
        Self::with_options(cfg, StateOptions::default())
    }

    pub fn with_options(cfg: Config, opts: StateOptions) -> anyhow::Result<Shared> {
        let server_id = cfg
            .proxy
            .id
            .as_deref()
            .and_then(ids::normalize_hex_id)
            .unwrap_or_else(|| ids::hash_id(&["server", &cfg.proxy.name]));
        let root_id = ids::hash_id(&["root", &server_id]);
        let tls_provider: Arc<dyn TlsProvider> = opts
            .tls_provider
            .unwrap_or_else(|| Arc::new(RustlsProvider));
        let factory: Arc<dyn BackendFactory> = opts
            .backend_factory
            .unwrap_or_else(|| Arc::new(JellyfinFactory));
        let registry = Arc::new(ServerRegistry::new());
        let backends =
            backend::build_all(&cfg, &registry, factory.as_ref(), tls_provider.as_ref())?;
        let data_dir = cfg.proxy.data_dir.clone();
        let (table, sessions) = match std::fs::create_dir_all(&data_dir) {
            Ok(()) => (
                ids::HashedIdTable::new(registry.clone()).with_persistence(&data_dir)?,
                auth::Sessions::new(Some(data_dir.join("sessions.json"))),
            ),
            Err(e) => {
                tracing::warn!(
                    "data_dir {} not writable ({e}); ids and sessions will not persist",
                    data_dir.display()
                );
                (
                    ids::HashedIdTable::new(registry.clone()),
                    auth::Sessions::new(None),
                )
            }
        };
        let ids: Arc<dyn IdMapper> = Arc::new(ids::MeteredIdMapper::new(table, registry.clone()));
        let sessions = Arc::new(sessions);
        let auth = Arc::new(users::MeteredAuthenticator(
            users::TomlAuthenticator::from_config(&cfg),
        ));
        sessions.retain_users(&auth.ids());
        let catalog = opts.catalog.unwrap_or_else(|| cache::build(&cfg.cache));
        let backends = Arc::new(ArcSwap::from_pointee(BackendSet::new(backends)));

        let st = Arc::new(AppState {
            cfg: RwLock::new(Arc::new(cfg)),
            server_id,
            root_id,
            backends,
            servers: registry,
            backend_factory: factory,
            ids,
            sessions,
            authenticator: auth,
            catalog,
            views_refresh: tokio::sync::Mutex::new(()),
            rewriters: opts.rewriters.unwrap_or_default(),
            endpoints: opts
                .endpoints
                .unwrap_or_else(endpoints::Registry::default_jellyfin),
            tls_provider,
            display_prefs: Mutex::new(HashMap::new()),
            started: Instant::now(),
            password_cache: opts.password_cache.unwrap_or_default(),
        });
        st.publish_config_gauges();
        Ok(st)
    }

    /// Current config snapshot.
    pub fn cfg(&self) -> Arc<Config> {
        self.cfg.read().unwrap().clone()
    }

    /// Snapshot of the live backends (config order, primary first). Hold
    /// it for the whole request so a concurrent reload cannot change it.
    pub fn backends(&self) -> Arc<BackendSet> {
        self.backends.load_full()
    }

    pub fn backend(&self, id: ServerId) -> Option<Arc<dyn MediaBackend>> {
        self.backends.load().get(id).cloned()
    }

    /// Whether `id` is served, was removed from the config, or was never known.
    pub fn server_state(&self, id: ServerId) -> ServerState {
        if let Some(b) = self.backend(id) {
            return ServerState::Active(b);
        }
        match self.servers.name(id) {
            Some(n) => ServerState::Removed(n),
            None => ServerState::Unknown,
        }
    }

    pub fn is_proxy_user(&self, normalized_id: &str) -> bool {
        self.authenticator.is_user_id(normalized_id)
    }

    pub fn password_cache(&self) -> &PasswordCache {
        &self.password_cache
    }

    fn publish_config_gauges(&self) {
        let cfg = self.cfg();
        gauge!(names::CONFIG_LAST_RELOAD).set(telemetry::unix_now());
        gauge!(names::CONFIG_UPSTREAMS)
            .set(cfg.upstreams.iter().filter(|u| u.enabled).count() as f64);
        gauge!(names::SERVERS_ACTIVE).set(self.backends.load().len() as f64);
        gauge!(names::CONFIG_USERS).set(cfg.users.len() as f64);
        if let Some(t) = &cfg.tls {
            if let Some(exp) = self.tls_provider.cert_expiry(&t.cert_path) {
                gauge!(names::TLS_CERT_EXPIRY, "cert" => "listener").set(exp as f64);
            }
        }
    }

    /// Log into all backends in the background (failures are only logged),
    /// and start the catalog refresher if `cache.refresh_interval_secs > 0`.
    pub fn spawn_initial_logins(self: &Shared) {
        for u in self.backends().iter() {
            let u = u.clone();
            tokio::spawn(async move {
                if let Err(e) = u.ensure_login().await {
                    tracing::warn!(upstream = %u.name(), "initial login failed: {e}");
                }
            });
        }
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                let secs = match weak.upgrade() {
                    Some(st) => st.cfg().cache.refresh_interval_secs,
                    None => return,
                };
                tokio::time::sleep(std::time::Duration::from_secs(secs.max(5))).await;
                if secs == 0 {
                    continue;
                }
                let Some(st) = weak.upgrade() else { return };
                if st.cfg().cache.enabled {
                    proxy::refresh_catalog(&st).await;
                }
            }
        });
    }
}

pub fn build_app(state: Shared) -> axum::Router {
    proxy::router(state)
}
