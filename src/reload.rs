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
    /// The writable config file behind this source, if there is one.
    fn path(&self) -> Option<&Path> {
        None
    }
}

/// The TOML file on disk (the proxy rewrites it only to hash/encrypt secrets
/// and to store a changed proxy-user password).
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
    fn path(&self) -> Option<&Path> {
        Some(&self.path)
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

mod subscribers;
mod watch;

pub use subscribers::*;
pub use watch::*;
