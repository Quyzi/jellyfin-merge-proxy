#![recursion_limit = "512"]
//! jellyfin-merge-proxy: present several Jellyfin servers as one.
//!
//! See `ARCHITECTURE.md` for the trait seams. In short, [`AppState`] holds
//! only trait objects / generic decorators:
//! * backends: an `ArcSwap` of [`servers::BackendSet`] (`Arc<dyn backend::MediaBackend>`s
//!   from a [`backend::BackendFactory`]), kept in sync with `[[server]]` by
//!   [`servers::BackendManager`];
//! * ids: `Arc<dyn ids::IdMapper>`; users: `Arc<dyn users::ProxyAuthenticator>`;
//! * catalog: `Arc<dyn cache::CatalogCache>`; TLS: `Arc<dyn tls::TlsProvider>`;
//! * config: `Arc<dyn reload::ConfigSource>` + a [`reload::ReloadHub`] of
//!   [`reload::Reloadable`] subscribers;
//! * HTTP: an [`endpoints::Registry`] and a [`rewrite::RewriteChain`].

pub mod auth;
pub mod backend;
pub mod cache;
pub mod config;
pub mod config_rewrite;
pub mod dto;
pub mod endpoints;
pub mod error;
pub mod ids;
pub mod index;
pub mod live;
pub mod local;
pub mod merge;
pub mod parental;
pub mod password;
pub mod paths;
pub mod proxy;
pub mod reload;
pub mod rewrite;
pub mod secrets;
pub mod server;
pub mod servers;
pub mod siblings;
mod state;
pub mod store;
pub mod telemetry;
pub mod tls;
pub mod types;
pub mod userdata;
pub mod users;

/// Backwards-compatible path for the old `upstream` module.
pub mod upstream {
    pub use crate::backend::jellyfin::version_at_least;
    pub use crate::backend::transport::{error_kind, HttpTransport, Instrumented, Transport};
    pub use crate::backend::{JellyfinBackend as Upstream, MediaBackend};
}

use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use arc_swap::ArcSwap;
use backend::{BackendFactory, JellyfinFactory, MediaBackend};
use cache::CatalogCache;
use config::{Config, PasswordCache};
use ids::IdMapper;
use reload::{ConfigSource, FileConfigSource, ReloadHub};
use servers::{BackendManager, BackendSet, ServerRegistry, ServerState};
use tls::{RustlsProvider, TlsProvider};
use types::ServerId;
use users::ProxyAuthenticator;

pub use reload::TlsReloader;

pub struct AppState {
    cfg: RwLock<Arc<Config>>,
    pub server_id: String,
    pub root_id: String,
    /// The enabled `[[server]]`s. Swapped atomically on reload; use
    /// [`AppState::backends`] to take a snapshot for one request.
    backends: Arc<ArcSwap<BackendSet>>,
    /// Every server name ever configured in this process, by [`ServerId`].
    pub servers: Arc<ServerRegistry>,
    pub backend_factory: Arc<dyn BackendFactory>,
    pub ids: Arc<dyn IdMapper>,
    /// The canonical catalog index.
    pub index: Arc<index::service::IndexService>,
    /// Per-proxy-user watch state.
    pub userdata: Arc<userdata::UserDataService>,
    pub seeder: Arc<userdata::seed::Seeder>,
    /// Per-user parental rating limits.
    pub parental: Arc<parental::service::ParentalService>,
    /// State the proxy owns itself (settings, preferences, playlists, ...).
    pub local: local::Local,
    /// Copies of one series/season on several servers.
    pub siblings: Arc<siblings::SiblingTable>,
    pub sessions: Arc<auth::Sessions>,
    pub authenticator: Arc<dyn ProxyAuthenticator>,
    pub catalog: Arc<dyn CatalogCache>,
    /// Serialises catalog refreshes.
    pub views_refresh: tokio::sync::Mutex<()>,
    pub rewriters: rewrite::RewriteChain,
    pub endpoints: endpoints::Registry,
    pub tls_provider: Arc<dyn TlsProvider>,
    pub hub: ReloadHub,
    pub listener_tls: Arc<reload::ListenerTls>,
    pub generation: AtomicU64,
    pub started: Instant,
    pub last_reload: Mutex<Option<(String, Option<String>)>>,
    password_cache: PasswordCache,
    pub source: Option<Arc<dyn ConfigSource>>,
    pub log: Option<telemetry::LogHandle>,
    reload_lock: tokio::sync::Mutex<()>,
}

pub type Shared = Arc<AppState>;

/// Optional overrides for every seam (all default to the production impls).
#[derive(Default)]
pub struct StateOptions {
    /// Shorthand for `source: FileConfigSource::new(path)`.
    pub config_path: Option<PathBuf>,
    pub source: Option<Arc<dyn ConfigSource>>,
    pub log: Option<telemetry::LogHandle>,
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
        for b in &backends {
            servers::set_configured(b.name(), true);
        }
        let data_dir = cfg.proxy.data_dir.clone();
        let store: Arc<dyn store::Store> = match std::fs::create_dir_all(&data_dir) {
            Ok(()) => store::open(&cfg.database, &data_dir)?,
            Err(e) => {
                tracing::warn!(
                    "data_dir {} not writable ({e}); ids and sessions will not persist",
                    data_dir.display()
                );
                Arc::new(store::MemoryStore::new())
            }
        };
        let table = ids::HashedIdTable::new(registry.clone()).with_store(store.clone())?;
        let sessions = auth::Sessions::with_store(store.clone())?;
        let siblings = siblings::SiblingTable::new().with_store(store.clone())?;
        let groups = Arc::new(index::groups::CanonicalGroups::new());
        let table = ids::PersistentViews::load(table, registry.clone(), store.clone())?;
        let grouped = Arc::new(ids::GroupedIdMapper::new(
            ids::MeteredIdMapper::new(table, registry.clone()),
            groups.clone(),
        ));
        let ids: Arc<dyn IdMapper> = grouped.clone();
        let index = Arc::new(index::service::IndexService::new(
            store.clone(),
            registry.clone(),
            groups.clone(),
            cfg.index.page_size,
        ));
        let play_sessions = Arc::new(live::play_sessions::PlaySessions::default());
        let mut rewriters = opts.rewriters.unwrap_or_default();
        rewriters.push(Box::new(rewrite::MeteredRewriter(
            live::play_sessions::PlaySessionRecorder(play_sessions.clone()),
        )));
        rewriters.push(Box::new(rewrite::MeteredRewriter(
            index::canonical::CanonicalIdRewriter::new(groups),
        )));
        let watch = userdata::MeteredWatchState::new(userdata::UserDataTable::load(store.clone()));
        let userdata = Arc::new(userdata::UserDataService::new(
            Arc::new(watch),
            index.clone(),
            &cfg.userdata,
        ));
        rewriters.push(Box::new(rewrite::MeteredRewriter(
            userdata::view::UserDataOverlay::new(userdata.clone()),
        )));
        let parental = Arc::new(parental::service::ParentalService::new(
            &cfg,
            index.clone(),
            ids.clone(),
        ));
        rewriters.push(Box::new(rewrite::MeteredRewriter(
            parental::filter::ParentalFilter::new(parental.clone()),
        )));
        let live_sessions = Arc::new(live::LiveSessions::new());
        let local = local::Local::new(store.clone(), play_sessions, live_sessions.clone());
        let seeder = Arc::new(userdata::seed::Seeder::new(store));
        let sessions = Arc::new(sessions);
        let auth = Arc::new(users::MeteredAuthenticator(
            users::TomlAuthenticator::from_config(&cfg),
        ));
        sessions.retain_users(&auth.ids());
        let catalog = opts.catalog.unwrap_or_else(|| cache::build(&cfg.cache));
        let source = opts.source.or_else(|| {
            opts.config_path
                .map(|p| Arc::new(FileConfigSource::new(p)) as Arc<dyn ConfigSource>)
        });
        let listener_tls = Arc::new(reload::ListenerTls::default());

        // Reload subscribers, in apply order.
        let backends = Arc::new(ArcSwap::from_pointee(BackendSet::new(backends)));
        let live = backends.clone();
        grouped.set_liveness(Arc::new(move |s| {
            live.load().get(s).is_some_and(|b| !b.is_down())
        }));
        let hub = ReloadHub::default();
        hub.subscribe(Arc::new(BackendManager::new(
            backends.clone(),
            registry.clone(),
            factory.clone(),
            catalog.clone(),
        )));
        hub.subscribe(auth.clone());
        hub.subscribe(Arc::new(auth::SessionPruner {
            sessions: sessions.clone(),
            users: auth.clone(),
            live: live_sessions,
        }));
        hub.subscribe(Arc::new(reload::LogFilter(opts.log.clone())));
        hub.subscribe(listener_tls.clone());
        hub.subscribe(Arc::new(reload::CatalogCacheReload(catalog.clone())));
        hub.subscribe(userdata.clone());
        hub.subscribe(parental.clone());
        hub.subscribe(Arc::new(reload::RestartNotices));

        let st = Arc::new(AppState {
            cfg: RwLock::new(Arc::new(cfg)),
            server_id,
            root_id,
            backends,
            servers: registry,
            backend_factory: factory,
            ids,
            index,
            userdata,
            seeder,
            parental,
            local,
            siblings: Arc::new(siblings),
            sessions,
            authenticator: auth,
            catalog,
            views_refresh: tokio::sync::Mutex::new(()),
            rewriters,
            endpoints: opts
                .endpoints
                .unwrap_or_else(endpoints::Registry::default_jellyfin),
            tls_provider,
            hub,
            listener_tls,
            generation: AtomicU64::new(1),
            started: Instant::now(),
            last_reload: Mutex::new(None),
            password_cache: opts.password_cache.unwrap_or_default(),
            source,
            log: opts.log,
            reload_lock: tokio::sync::Mutex::new(()),
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
}

pub fn build_app(state: Shared) -> axum::Router {
    proxy::router(state)
}
