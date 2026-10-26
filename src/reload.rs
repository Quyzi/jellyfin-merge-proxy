//! Config lifecycle: where config comes from ([`ConfigSource`]), what
//! triggers a reload ([`ConfigWatcher`]), and who reacts ([`Reloadable`]
//! subscribers registered on a [`ReloadHub`]).
//!
//! A reload is two-phase: every subscriber `validate`s the new config first;
//! only if all succeed does any subscriber `apply` it. A failed validation
//! leaves the running config untouched.

use crate::config::{Config, PasswordCache};
use crate::error::{ConfigError, ReloadError};
use crate::telemetry::names;
use crate::tls::TlsProvider;
use crate::Shared;
use async_trait::async_trait;
use metrics::counter;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

/// Produces a parsed [`Config`]. Never writes anything back.
pub trait ConfigSource: Send + Sync {
    /// Human-readable origin (shown on the debug endpoint and in logs).
    fn describe(&self) -> String;
    fn load(&self, cache: &PasswordCache) -> Result<Config, ConfigError>;
    /// Files whose modification should trigger a reload.
    fn watched_paths(&self, cfg: &Config) -> Vec<PathBuf> {
        cfg.watched_files()
    }
}

/// The TOML file on disk (read-only; the proxy never rewrites it).
#[derive(Debug, Clone)]
pub struct FileConfigSource {
    pub path: PathBuf,
}

impl FileConfigSource {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        FileConfigSource { path: path.into() }
    }
}

impl ConfigSource for FileConfigSource {
    fn describe(&self) -> String {
        self.path.display().to_string()
    }
    fn load(&self, cache: &PasswordCache) -> Result<Config, ConfigError> {
        Config::load(&self.path, cache)
    }
    fn watched_paths(&self, cfg: &Config) -> Vec<PathBuf> {
        let mut v = vec![self.path.clone()];
        v.extend(cfg.watched_files());
        v
    }
}

/// In-memory TOML text (tests, embedding). `set` swaps the text.
#[derive(Debug)]
pub struct StaticConfigSource {
    text: Mutex<String>,
}

impl StaticConfigSource {
    pub fn new(text: impl Into<String>) -> Self {
        StaticConfigSource {
            text: Mutex::new(text.into()),
        }
    }
    pub fn set(&self, text: impl Into<String>) {
        *self.text.lock().unwrap() = text.into();
    }
}

impl ConfigSource for StaticConfigSource {
    fn describe(&self) -> String {
        "<static>".into()
    }
    fn load(&self, cache: &PasswordCache) -> Result<Config, ConfigError> {
        Config::parse(&self.text.lock().unwrap(), cache)
    }
}

// ---------------------------------------------------------------------------
// Subscribers
// ---------------------------------------------------------------------------

/// Everything a subscriber needs to react to a reload.
pub struct ReloadContext<'a> {
    pub old: &'a Config,
    pub new: &'a Config,
    /// Referenced files (certificates, keys) changed on disk.
    pub files_changed: bool,
    pub tls: &'a dyn TlsProvider,
}

/// A component that reacts to config changes.
#[async_trait]
pub trait Reloadable: Send + Sync {
    /// Name for logs (e.g. "backend:home", "listener-tls").
    fn reload_name(&self) -> String;
    /// Check the new config can be applied. Must not change state.
    fn validate(&self, _cx: &ReloadContext<'_>) -> Result<(), ReloadError> {
        Ok(())
    }
    /// Apply the new config. Returns true if anything changed.
    async fn apply(&self, cx: &ReloadContext<'_>) -> Result<bool, ReloadError>;
}

/// Ordered list of subscribers (dyn: the set is built at runtime).
#[derive(Default)]
pub struct ReloadHub {
    subs: RwLock<Vec<Arc<dyn Reloadable>>>,
}

impl ReloadHub {
    pub fn subscribe(&self, s: Arc<dyn Reloadable>) {
        self.subs.write().unwrap().push(s);
    }
    pub fn names(&self) -> Vec<String> {
        self.subs
            .read()
            .unwrap()
            .iter()
            .map(|s| s.reload_name())
            .collect()
    }
    /// Validate with every subscriber, then apply with every subscriber.
    pub async fn run(&self, cx: &ReloadContext<'_>) -> Result<bool, ReloadError> {
        let subs = self.subs.read().unwrap().clone();
        for s in &subs {
            s.validate(cx).inspect_err(|_| {
                tracing::debug!(subscriber = %s.reload_name(), "reload validation failed");
            })?;
        }
        let mut changed = false;
        let mut first_err = None;
        for s in &subs {
            match s.apply(cx).await {
                Ok(c) => changed |= c,
                Err(e) => {
                    tracing::error!(subscriber = %s.reload_name(), "applying reload: {e}");
                    first_err.get_or_insert(e);
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(changed),
        }
    }
}

/// Hook that pushes a new rustls config into the running listener.
pub type TlsReloader = Box<dyn Fn(Arc<rustls::ServerConfig>) + Send + Sync>;

/// Hot-swaps the listener certificate.
#[derive(Default)]
pub struct ListenerTls {
    pub reloader: Mutex<Option<TlsReloader>>,
}

#[async_trait]
impl Reloadable for ListenerTls {
    fn reload_name(&self) -> String {
        "listener-tls".into()
    }
    fn validate(&self, cx: &ReloadContext<'_>) -> Result<(), ReloadError> {
        if let Some(t) = &cx.new.tls {
            if let Err(e) = cx.tls.listener(t) {
                counter!(names::TLS_RELOADS, "target" => "listener", "outcome" => "error")
                    .increment(1);
                return Err(ReloadError::Tls {
                    what: "[tls]".into(),
                    source: e,
                });
            }
        }
        Ok(())
    }
    async fn apply(&self, cx: &ReloadContext<'_>) -> Result<bool, ReloadError> {
        if cx.old.proxy.listen != cx.new.proxy.listen
            || cx.old.tls.is_some() != cx.new.tls.is_some()
        {
            tracing::warn!(
                "proxy.listen / enabling or disabling [tls] needs a restart; other changes applied"
            );
        }
        let Some(t) = &cx.new.tls else {
            return Ok(false);
        };
        if cx.old.tls.as_ref() == Some(t) && !cx.files_changed {
            return Ok(false);
        }
        let sc = cx.tls.listener(t).map_err(|e| ReloadError::Tls {
            what: "[tls]".into(),
            source: e,
        })?;
        let applied = self
            .reloader
            .lock()
            .unwrap()
            .as_ref()
            .map(|f| f(sc))
            .is_some();
        if applied {
            counter!(names::TLS_RELOADS, "target" => "listener", "outcome" => "success")
                .increment(1);
            tracing::info!("listener TLS certificate reloaded");
        }
        Ok(applied)
    }
}

/// Applies `[logging] filter` to the live subscriber.
pub struct LogFilter(pub Option<crate::telemetry::LogHandle>);

#[async_trait]
impl Reloadable for LogFilter {
    fn reload_name(&self) -> String {
        "log-filter".into()
    }
    async fn apply(&self, cx: &ReloadContext<'_>) -> Result<bool, ReloadError> {
        if cx.old.logging.format != cx.new.logging.format
            || cx.old.logging.ansi != cx.new.logging.ansi
        {
            tracing::warn!("logging.format / logging.ansi changes need a restart");
        }
        if cx.old.logging.filter == cx.new.logging.filter {
            return Ok(false);
        }
        if let Some(l) = &self.0 {
            l.set_filter(&cx.new.logging.filter)
                .map_err(|e| ReloadError::Other(format!("logging.filter: {e:#}")))?;
        }
        Ok(true)
    }
}

/// Warns about settings that only take effect after a restart.
pub struct RestartNotices;

#[async_trait]
impl Reloadable for RestartNotices {
    fn reload_name(&self) -> String {
        "restart-notices".into()
    }
    async fn apply(&self, cx: &ReloadContext<'_>) -> Result<bool, ReloadError> {
        let (o, n) = (cx.old, cx.new);
        if o.metrics.listen != n.metrics.listen
            || o.metrics.path != n.metrics.path
            || o.debug.path != n.debug.path
            || o.debug.enabled != n.debug.enabled
            || o.metrics.enabled != n.metrics.enabled
        {
            tracing::warn!("metrics/debug endpoint changes need a restart");
        }
        if o.cache.enabled != n.cache.enabled || o.cache.persist_path != n.cache.persist_path {
            tracing::warn!("cache.enabled / cache.persist_path changes need a restart");
        }
        if o.proxy.data_dir != n.proxy.data_dir {
            tracing::warn!("proxy.data_dir changes need a restart");
        }
        if o.proxy.name != n.proxy.name || o.proxy.id != n.proxy.id {
            tracing::warn!(
                "proxy.name / proxy.id (the server id clients see) change after a restart"
            );
        }
        // Adding/removing [[server]]s and URL changes are applied live by
        // `servers::BackendManager`.
        Ok(false)
    }
}

/// Re-tunes / invalidates the catalog cache.
pub struct CatalogCacheReload(pub Arc<dyn crate::cache::CatalogCache>);

#[async_trait]
impl Reloadable for CatalogCacheReload {
    fn reload_name(&self) -> String {
        "catalog-cache".into()
    }
    async fn apply(&self, cx: &ReloadContext<'_>) -> Result<bool, ReloadError> {
        let mut changed = false;
        if cx.old.cache != cx.new.cache {
            self.0.configure((&cx.new.cache).into());
            changed = true;
        }
        // Per-server entries are purged by `servers::BackendManager`.
        if cx.old.merge != cx.new.merge {
            self.0.invalidate_all();
            changed = true;
        }
        Ok(changed)
    }
}

// ---------------------------------------------------------------------------
// Watchers
// ---------------------------------------------------------------------------

/// A detected change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Change {
    /// Referenced files (not just the config text) changed.
    pub files_changed: bool,
}

/// Something that tells us when to reload.
#[async_trait]
pub trait ConfigWatcher: Send {
    fn name(&self) -> &'static str;
    /// Wait for the next change; `None` ends the watcher.
    async fn next_change(&mut self, st: &Shared) -> Option<Change>;
}

/// File fingerprint: modification time and size (catches editors that keep
/// the mtime, and same-second edits that change the length).
type Stamp = Option<(SystemTime, u64)>;

/// Polls the config file and every file it references every
/// `[proxy] reload_interval_secs` (default 3, 0 = paused; SIGHUP still works).
pub struct PollingWatcher {
    snapshot: Option<Vec<(PathBuf, Stamp)>>,
}

impl PollingWatcher {
    pub fn new() -> Self {
        PollingWatcher { snapshot: None }
    }
    fn take(st: &Shared) -> Vec<(PathBuf, Stamp)> {
        let Some(src) = &st.source else {
            return vec![];
        };
        src.watched_paths(&st.cfg())
            .into_iter()
            .map(|p| {
                let m = stamp(&p);
                (p, m)
            })
            .collect()
    }
}

impl Default for PollingWatcher {
    fn default() -> Self {
        Self::new()
    }
}

fn stamp(p: &Path) -> Stamp {
    let m = std::fs::metadata(p).ok()?;
    Some((m.modified().ok()?, m.len()))
}

#[async_trait]
impl ConfigWatcher for PollingWatcher {
    fn name(&self) -> &'static str {
        "poll"
    }
    async fn next_change(&mut self, st: &Shared) -> Option<Change> {
        st.source.as_ref()?;
        if self.snapshot.is_none() {
            self.snapshot = Some(Self::take(st));
        }
        loop {
            let secs = st.cfg().proxy.reload_interval_secs;
            tokio::time::sleep(Duration::from_secs(secs.max(1))).await;
            if secs == 0 {
                continue;
            }
            let now = Self::take(st);
            let before = self.snapshot.as_ref().unwrap();
            // Compare only paths present in both snapshots; a changed path
            // *set* (after a reload) just re-baselines.
            let mut cfg_changed = false;
            let mut files_changed = false;
            for (i, (p, m)) in now.iter().enumerate() {
                if let Some((_, om)) = before.iter().find(|(op, _)| op == p) {
                    if om != m {
                        if i == 0 {
                            cfg_changed = true;
                        } else {
                            files_changed = true;
                        }
                    }
                }
            }
            self.snapshot = Some(now);
            if cfg_changed || files_changed {
                tracing::info!(
                    config_changed = cfg_changed,
                    files_changed,
                    "change detected, reloading"
                );
                return Some(Change { files_changed });
            }
        }
    }
}

/// Reloads on SIGHUP (unix only; never fires elsewhere).
pub struct SighupWatcher {
    #[cfg(unix)]
    sig: Option<tokio::signal::unix::Signal>,
}

impl SighupWatcher {
    pub fn new() -> Self {
        SighupWatcher {
            #[cfg(unix)]
            sig: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()).ok(),
        }
    }
}

impl Default for SighupWatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ConfigWatcher for SighupWatcher {
    fn name(&self) -> &'static str {
        "sighup"
    }
    async fn next_change(&mut self, _st: &Shared) -> Option<Change> {
        #[cfg(unix)]
        {
            self.sig.as_mut()?.recv().await?;
            tracing::info!("SIGHUP: reloading config");
            Some(Change {
                files_changed: true,
            })
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
}

/// Drive a watcher: every change triggers `st.reload`.
pub fn spawn_watcher(mut w: Box<dyn ConfigWatcher>, st: Shared) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tracing::debug!(watcher = w.name(), "config watcher started");
        while let Some(c) = w.next_change(&st).await {
            let _ = st.reload(c.files_changed).await;
        }
    })
}
