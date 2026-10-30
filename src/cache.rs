//! Catalog cache: slow-changing per-server catalog data (library views).
//!
//! [`CatalogCache`] is the seam. Implementations:
//! * [`MemoryCatalogCache`]: TTL + max-entries, oldest-first eviction;
//! * [`DiskCatalogCache`]: wraps another cache and persists it as JSON at
//!   `[cache] persist_path`, so a restart starts warm;
//! * [`NoCache`]: `[cache] enabled = false`;
//! * [`MeteredCache`]: decorator adding the `jmp_cache_*` metrics.
//!
//! Configured by the `[cache]` TOML section (see [`build`]).

use crate::config::CacheConfig;
use crate::telemetry::{self, names};
use metrics::{counter, gauge};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// What is cached, for which server.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CacheKey {
    /// Catalog kind, e.g. "views". Bounded (used as a metric label).
    pub kind: String,
    /// Upstream server name.
    pub server: String,
}

impl CacheKey {
    pub fn new(kind: &str, server: &str) -> Self {
        CacheKey {
            kind: kind.to_string(),
            server: server.to_string(),
        }
    }
}

/// Result of a lookup.
#[derive(Debug, Clone, PartialEq)]
pub enum Lookup {
    Fresh(Value),
    /// Present but older than the TTL; callers refresh and may fall back to it.
    Stale(Value),
    Miss,
}

/// Outcome of a store, for metrics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PutOutcome {
    pub replaced: bool,
    pub evicted: usize,
    pub bytes: usize,
}

/// Runtime-tunable settings (hot-reloadable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheSettings {
    pub ttl: Duration,
    pub max_entries: usize,
    pub serve_stale_on_error: bool,
}

impl From<&CacheConfig> for CacheSettings {
    fn from(c: &CacheConfig) -> Self {
        CacheSettings {
            ttl: Duration::from_secs(c.ttl_secs),
            max_entries: c.max_entries.max(1),
            serve_stale_on_error: c.serve_stale_on_error,
        }
    }
}

pub trait CatalogCache: Send + Sync {
    fn name(&self) -> &'static str;
    fn get(&self, key: &CacheKey) -> Lookup;
    fn put(&self, key: CacheKey, value: Value) -> PutOutcome;
    fn invalidate_all(&self);
    /// Drop every entry for one server (e.g. it was removed or rebuilt).
    /// Returns how many entries were dropped.
    fn invalidate_server(&self, server: &str) -> usize;
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn settings(&self) -> CacheSettings;
    fn configure(&self, s: CacheSettings);
}

/// `[cache] enabled = false`.
#[derive(Debug)]
pub struct NoCache;

impl CatalogCache for NoCache {
    fn name(&self) -> &'static str {
        "none"
    }
    fn get(&self, _: &CacheKey) -> Lookup {
        Lookup::Miss
    }
    fn put(&self, _: CacheKey, _: Value) -> PutOutcome {
        PutOutcome::default()
    }
    fn invalidate_all(&self) {}
    fn invalidate_server(&self, _: &str) -> usize {
        0
    }
    fn len(&self) -> usize {
        0
    }
    fn settings(&self) -> CacheSettings {
        CacheSettings {
            ttl: Duration::ZERO,
            max_entries: 0,
            serve_stale_on_error: false,
        }
    }
    fn configure(&self, _: CacheSettings) {}
}

#[derive(Debug, Clone)]
struct Entry {
    value: Value,
    stored: Instant,
    stored_unix: f64,
}

/// In-memory TTL cache with a max-entries bound.
#[derive(Debug)]
pub struct MemoryCatalogCache {
    map: RwLock<HashMap<CacheKey, Entry>>,
    settings: RwLock<CacheSettings>,
}

impl MemoryCatalogCache {
    pub fn new(s: CacheSettings) -> Self {
        MemoryCatalogCache {
            map: RwLock::new(HashMap::new()),
            settings: RwLock::new(s),
        }
    }

    fn insert_aged(&self, key: CacheKey, value: Value, stored_unix: f64) {
        let age = (telemetry::unix_now() - stored_unix).max(0.0);
        let stored = Instant::now()
            .checked_sub(Duration::from_secs_f64(age))
            .unwrap_or_else(Instant::now);
        self.map.write().unwrap().insert(
            key,
            Entry {
                value,
                stored,
                stored_unix,
            },
        );
    }

    fn snapshot(&self) -> Vec<PersistedEntry> {
        self.map
            .read()
            .unwrap()
            .iter()
            .map(|(k, e)| PersistedEntry {
                key: k.clone(),
                stored_unix: e.stored_unix,
                value: e.value.clone(),
            })
            .collect()
    }
}

impl CatalogCache for MemoryCatalogCache {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn get(&self, key: &CacheKey) -> Lookup {
        let ttl = self.settings.read().unwrap().ttl;
        match self.map.read().unwrap().get(key) {
            Some(e) if e.stored.elapsed() < ttl => Lookup::Fresh(e.value.clone()),
            Some(e) => Lookup::Stale(e.value.clone()),
            None => Lookup::Miss,
        }
    }
    fn put(&self, key: CacheKey, value: Value) -> PutOutcome {
        let max = self.settings.read().unwrap().max_entries;
        let bytes = serde_json::to_vec(&value).map(|b| b.len()).unwrap_or(0);
        let mut m = self.map.write().unwrap();
        let replaced = m
            .insert(
                key,
                Entry {
                    value,
                    stored: Instant::now(),
                    stored_unix: telemetry::unix_now(),
                },
            )
            .is_some();
        let mut evicted = 0;
        while m.len() > max {
            let Some(oldest) = m
                .iter()
                .min_by_key(|(_, e)| e.stored)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            m.remove(&oldest);
            evicted += 1;
        }
        PutOutcome {
            replaced,
            evicted,
            bytes,
        }
    }
    fn invalidate_all(&self) {
        self.map.write().unwrap().clear();
    }
    fn invalidate_server(&self, server: &str) -> usize {
        let mut m = self.map.write().unwrap();
        let before = m.len();
        m.retain(|k, _| !k.server.eq_ignore_ascii_case(server));
        before - m.len()
    }
    fn len(&self) -> usize {
        self.map.read().unwrap().len()
    }
    fn settings(&self) -> CacheSettings {
        *self.settings.read().unwrap()
    }
    fn configure(&self, s: CacheSettings) {
        *self.settings.write().unwrap() = s;
    }
}

#[derive(Serialize, Deserialize)]
struct PersistedEntry {
    key: CacheKey,
    stored_unix: f64,
    value: Value,
}

/// Persists a [`MemoryCatalogCache`] to a JSON file (written atomically on
/// every store; loaded at startup, keeping each entry's original age).
pub struct DiskCatalogCache {
    inner: MemoryCatalogCache,
    path: PathBuf,
}

impl DiskCatalogCache {
    pub fn open(path: PathBuf, s: CacheSettings) -> Self {
        let inner = MemoryCatalogCache::new(s);
        match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Vec<PersistedEntry>>(&bytes) {
                Ok(entries) => {
                    let n = entries.len();
                    for e in entries {
                        inner.insert_aged(e.key, e.value, e.stored_unix);
                    }
                    tracing::info!("loaded {n} catalog cache entries from {}", path.display());
                    counter!(names::PERSIST_OPS, "store" => "catalog", "op" => "load", "outcome" => "success").increment(1);
                }
                Err(e) => {
                    tracing::warn!("ignoring unreadable catalog cache {}: {e}", path.display());
                    counter!(names::PERSIST_OPS, "store" => "catalog", "op" => "load", "outcome" => "error").increment(1);
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!("cannot read catalog cache {}: {e}", path.display());
                counter!(names::PERSIST_OPS, "store" => "catalog", "op" => "load", "outcome" => "error").increment(1);
            }
        }
        DiskCatalogCache { inner, path }
    }

    fn save(&self) {
        let res = (|| -> std::io::Result<()> {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = self.path.with_extension("tmp");
            std::fs::write(&tmp, serde_json::to_vec(&self.inner.snapshot())?)?;
            std::fs::rename(&tmp, &self.path)
        })();
        let outcome = if res.is_ok() { "success" } else { "error" };
        if let Err(e) = res {
            tracing::warn!("writing catalog cache {}: {e}", self.path.display());
        }
        counter!(names::PERSIST_OPS, "store" => "catalog", "op" => "save", "outcome" => outcome)
            .increment(1);
    }
}

impl CatalogCache for DiskCatalogCache {
    fn name(&self) -> &'static str {
        "disk"
    }
    fn get(&self, key: &CacheKey) -> Lookup {
        self.inner.get(key)
    }
    fn put(&self, key: CacheKey, value: Value) -> PutOutcome {
        let o = self.inner.put(key, value);
        self.save();
        o
    }
    fn invalidate_all(&self) {
        self.inner.invalidate_all();
        self.save();
    }
    fn invalidate_server(&self, server: &str) -> usize {
        let n = self.inner.invalidate_server(server);
        if n > 0 {
            self.save();
        }
        n
    }
    fn len(&self) -> usize {
        self.inner.len()
    }
    fn settings(&self) -> CacheSettings {
        self.inner.settings()
    }
    fn configure(&self, s: CacheSettings) {
        self.inner.configure(s)
    }
}

/// Decorator adding `jmp_cache_requests_total`, `jmp_cache_entries`,
/// `jmp_cache_bytes`, `jmp_cache_last_refresh_timestamp_seconds` and
/// `jmp_cache_evictions_total` to any [`CatalogCache`].
pub struct MeteredCache<C>(pub C);

impl<C: CatalogCache> CatalogCache for MeteredCache<C> {
    fn name(&self) -> &'static str {
        self.0.name()
    }
    fn get(&self, key: &CacheKey) -> Lookup {
        let r = self.0.get(key);
        let outcome = match &r {
            Lookup::Fresh(_) => "hit",
            Lookup::Stale(_) => "stale",
            Lookup::Miss => "miss",
        };
        counter!(names::CACHE_REQUESTS, "cache" => key.kind.clone(), "outcome" => outcome)
            .increment(1);
        r
    }
    fn put(&self, key: CacheKey, value: Value) -> PutOutcome {
        let entries = value.as_array().map(|a| a.len()).unwrap_or(1);
        let (kind, server) = (key.kind.clone(), key.server.clone());
        let o = self.0.put(key, value);
        gauge!(names::CACHE_ENTRIES, "cache" => kind.clone(), "server" => server.clone())
            .set(entries as f64);
        gauge!(names::CACHE_BYTES, "cache" => kind.clone(), "server" => server.clone())
            .set(o.bytes as f64);
        gauge!(names::CACHE_LAST_REFRESH, "cache" => kind.clone(), "server" => server)
            .set(telemetry::unix_now());
        let ev = o.evicted as u64 + u64::from(o.replaced);
        if ev > 0 {
            counter!(names::CACHE_EVICTIONS, "cache" => kind).increment(ev);
        }
        o
    }
    fn invalidate_all(&self) {
        self.0.invalidate_all()
    }
    fn invalidate_server(&self, server: &str) -> usize {
        let n = self.0.invalidate_server(server);
        gauge!(names::CACHE_ENTRIES, "cache" => "views", "server" => server.to_string()).set(0.0);
        gauge!(names::CACHE_BYTES, "cache" => "views", "server" => server.to_string()).set(0.0);
        if n > 0 {
            counter!(names::CACHE_EVICTIONS, "cache" => "views").increment(n as u64);
        }
        n
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn settings(&self) -> CacheSettings {
        self.0.settings()
    }
    fn configure(&self, s: CacheSettings) {
        self.0.configure(s)
    }
}

/// Build the cache described by `[cache]`.
pub fn build(cfg: &CacheConfig) -> Arc<dyn CatalogCache> {
    let s = CacheSettings::from(cfg);
    if !cfg.enabled {
        return Arc::new(MeteredCache(NoCache));
    }
    match &cfg.persist_path {
        Some(p) => Arc::new(MeteredCache(DiskCatalogCache::open(p.clone(), s))),
        None => Arc::new(MeteredCache(MemoryCatalogCache::new(s))),
    }
}

#[cfg(test)]
mod tests;
