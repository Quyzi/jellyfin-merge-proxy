//! The live server set and how it follows the config.
//!
//! * [`ServerRegistry`]: append-only `name ⇄ ServerId` table. Ids are never
//!   reused, so an id handed out for a server that was later removed still
//!   resolves to *that* server (and is answered with 410 Gone) instead of
//!   pointing at some other server.
//! * [`BackendSet`]: the enabled backends in config order. `AppState` keeps
//!   it in an `ArcSwap`: a request loads one snapshot and finishes on it, so
//!   a reload never changes the set under an in-flight request.
//! * [`BackendManager`]: the `Reloadable` that diffs the `[[server]]` list on
//!   every reload — builds and logs in new servers through the
//!   [`BackendFactory`], rebuilds servers whose `server` URL changed, applies
//!   in-place changes (TLS, credentials, timeouts) to the rest, purges the
//!   catalog cache of anything added/removed/rebuilt, swaps the set
//!   atomically and drains retired backends in the background.

use crate::backend::{BackendFactory, MediaBackend};
use crate::cache::CatalogCache;
use crate::config::{Config, UpstreamConfig};
use crate::error::ReloadError;
use crate::reload::{ReloadContext, Reloadable};
use crate::telemetry::names;
use crate::types::ServerId;
use arc_swap::ArcSwap;
use async_trait::async_trait;
use metrics::{counter, gauge, histogram};
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

/// Append-only `name ⇄ ServerId` table (names compared case-insensitively).
#[derive(Debug, Default)]
pub struct ServerRegistry {
    names: RwLock<Vec<String>>,
}

impl ServerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry with `names` registered in order (ids 0, 1, ...).
    pub fn with_names<S: AsRef<str>>(names: impl IntoIterator<Item = S>) -> Arc<Self> {
        let r = Self::new();
        for n in names {
            r.id_for(n.as_ref());
        }
        Arc::new(r)
    }

    /// The id for `name`, registering it if it is new.
    pub fn id_for(&self, name: &str) -> ServerId {
        if let Some(id) = self.lookup(name) {
            return id;
        }
        let mut n = self.names.write().unwrap();
        if let Some(i) = n.iter().position(|x| x.eq_ignore_ascii_case(name)) {
            return ServerId(i);
        }
        n.push(name.to_string());
        ServerId(n.len() - 1)
    }

    /// The id `name` has or would get next, without registering it.
    pub fn peek_ids<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> Vec<ServerId> {
        let n = self.names.read().unwrap();
        let mut next = n.len();
        names
            .into_iter()
            .map(
                |name| match n.iter().position(|x| x.eq_ignore_ascii_case(name)) {
                    Some(i) => ServerId(i),
                    None => {
                        next += 1;
                        ServerId(next - 1)
                    }
                },
            )
            .collect()
    }

    pub fn lookup(&self, name: &str) -> Option<ServerId> {
        self.names
            .read()
            .unwrap()
            .iter()
            .position(|x| x.eq_ignore_ascii_case(name))
            .map(ServerId)
    }

    pub fn name(&self, id: ServerId) -> Option<String> {
        self.names.read().unwrap().get(id.0).cloned()
    }

    /// All names ever registered, indexed by `ServerId`.
    pub fn names(&self) -> Vec<String> {
        self.names.read().unwrap().clone()
    }

    pub fn len(&self) -> usize {
        self.names.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The enabled backends, in config order (index 0 is the primary).
#[derive(Default)]
pub struct BackendSet {
    list: Vec<Arc<dyn MediaBackend>>,
}

impl BackendSet {
    pub fn new(list: Vec<Arc<dyn MediaBackend>>) -> Self {
        BackendSet { list }
    }
    pub fn get(&self, id: ServerId) -> Option<&Arc<dyn MediaBackend>> {
        self.list.iter().find(|b| b.id() == id)
    }
    pub fn by_name(&self, name: &str) -> Option<&Arc<dyn MediaBackend>> {
        self.list
            .iter()
            .find(|b| b.name().eq_ignore_ascii_case(name))
    }
    pub fn names(&self) -> Vec<String> {
        self.list.iter().map(|b| b.name().to_string()).collect()
    }
}

impl Deref for BackendSet {
    type Target = [Arc<dyn MediaBackend>];
    fn deref(&self) -> &Self::Target {
        &self.list
    }
}

/// Whether a [`ServerId`] is currently served.
pub enum ServerState {
    Active(Arc<dyn MediaBackend>),
    /// Known (ids were issued for it) but no longer in the config.
    Removed(String),
    Unknown,
}

/// What a reload does to one server.
enum Step {
    Keep(Arc<dyn MediaBackend>),
    /// The `server` URL changed: replace the old backend with a new one.
    Rebuild(Arc<dyn MediaBackend>),
    Add,
}

fn same_url(b: &dyn MediaBackend, cfg: &UpstreamConfig) -> bool {
    b.location().base == cfg.url.trim_end_matches('/')
}

fn enabled(cfg: &Config) -> impl Iterator<Item = &UpstreamConfig> {
    cfg.upstreams.iter().filter(|u| u.enabled)
}

/// Keeps the live [`BackendSet`] in sync with `[[server]]` (see module docs).
pub struct BackendManager {
    set: Arc<ArcSwap<BackendSet>>,
    registry: Arc<ServerRegistry>,
    factory: Arc<dyn BackendFactory>,
    catalog: Arc<dyn CatalogCache>,
    /// Backends built during `validate`, reused by `apply`.
    prepared: Mutex<HashMap<String, Arc<dyn MediaBackend>>>,
}

impl BackendManager {
    pub fn new(
        set: Arc<ArcSwap<BackendSet>>,
        registry: Arc<ServerRegistry>,
        factory: Arc<dyn BackendFactory>,
        catalog: Arc<dyn CatalogCache>,
    ) -> Self {
        BackendManager {
            set,
            registry,
            factory,
            catalog,
            prepared: Mutex::new(HashMap::new()),
        }
    }

    fn plan(&self, cur: &BackendSet, new: &Config) -> Vec<(UpstreamConfig, Step)> {
        enabled(new)
            .map(|u| {
                let step = match cur.by_name(&u.name) {
                    Some(b) if same_url(b.as_ref(), u) => Step::Keep(b.clone()),
                    Some(b) => Step::Rebuild(b.clone()),
                    None => Step::Add,
                };
                (u.clone(), step)
            })
            .collect()
    }
}

#[async_trait]
impl Reloadable for BackendManager {
    fn reload_name(&self) -> String {
        "backends".into()
    }

    /// Kept servers validate their own changes; new and rebuilt ones are
    /// constructed (not logged in) so URL/TLS errors reject the reload.
    fn validate(&self, cx: &ReloadContext<'_>) -> Result<(), ReloadError> {
        let cur = self.set.load_full();
        let plan = self.plan(&cur, cx.new);
        let ids = self
            .registry
            .peek_ids(plan.iter().map(|(u, _)| u.name.as_str()));
        let mut prepared = HashMap::new();
        for ((u, step), id) in plan.iter().zip(ids) {
            match step {
                Step::Keep(b) => b.validate(cx)?,
                Step::Add | Step::Rebuild(_) => {
                    let b = self.factory.create(id, u, cx.new, cx.tls)?;
                    prepared.insert(u.name.to_ascii_lowercase(), b);
                }
            }
        }
        *self.prepared.lock().unwrap() = prepared;
        Ok(())
    }

    async fn apply(&self, cx: &ReloadContext<'_>) -> Result<bool, ReloadError> {
        let cur = self.set.load_full();
        let plan = self.plan(&cur, cx.new);
        let mut prepared = std::mem::take(&mut *self.prepared.lock().unwrap());
        let mut list: Vec<Arc<dyn MediaBackend>> = Vec::with_capacity(plan.len());
        let mut retired: Vec<Arc<dyn MediaBackend>> = vec![];
        let mut fresh: Vec<Arc<dyn MediaBackend>> = vec![];
        let mut changed = false;
        let mut first_err = None;
        for (u, step) in plan {
            match step {
                Step::Keep(b) => {
                    match b.apply(cx).await {
                        Ok(true) => {
                            changed = true;
                            self.catalog.invalidate_server(b.name());
                        }
                        Ok(false) => {}
                        Err(e) => {
                            first_err.get_or_insert(e);
                        }
                    }
                    list.push(b);
                }
                Step::Add | Step::Rebuild(_) => {
                    let id = self.registry.id_for(&u.name);
                    let built = match prepared.remove(&u.name.to_ascii_lowercase()) {
                        Some(b) if b.id() == id => Ok(b),
                        _ => self.factory.create(id, &u, cx.new, cx.tls),
                    };
                    let b = match built {
                        Ok(b) => b,
                        Err(e) => {
                            tracing::error!(upstream = %u.name, "cannot build server: {e}");
                            first_err.get_or_insert(e.into());
                            // Keep the old backend (if any) rather than lose the server.
                            if let Step::Rebuild(old) = step {
                                list.push(old);
                            }
                            continue;
                        }
                    };
                    changed = true;
                    self.catalog.invalidate_server(&u.name);
                    match step {
                        Step::Rebuild(old) => {
                            counter!(names::CONFIG_UPSTREAM_CHANGES, "change" => "rebuilt")
                                .increment(1);
                            tracing::info!(upstream = %u.name, "[[server]] URL changed: rebuilt backend");
                            retired.push(old);
                        }
                        _ => {
                            counter!(names::CONFIG_UPSTREAM_CHANGES, "change" => "added")
                                .increment(1);
                            tracing::info!(upstream = %u.name, id = %id, "[[server]] added");
                        }
                    }
                    fresh.push(b.clone());
                    list.push(b);
                }
            }
        }
        for old in cur.iter() {
            if !list
                .iter()
                .any(|b| b.name().eq_ignore_ascii_case(old.name()))
            {
                changed = true;
                counter!(names::CONFIG_UPSTREAM_CHANGES, "change" => "removed").increment(1);
                tracing::info!(upstream = %old.name(), "[[server]] removed");
                self.catalog.invalidate_server(old.name());
                gauge!(names::UPSTREAM_UP, "server" => old.name().to_string()).set(0.0);
                retired.push(old.clone());
            }
        }
        if changed {
            // Atomic swap: requests that already loaded `cur` finish on it.
            self.set.store(Arc::new(BackendSet::new(list)));
        }
        gauge!(names::SERVERS_ACTIVE).set(self.set.load().len() as f64);
        for b in fresh {
            tokio::spawn(async move {
                if let Err(e) = b.ensure_login().await {
                    tracing::warn!(upstream = %b.name(), "login failed: {e}");
                }
            });
        }
        let grace = Duration::from_secs(cx.new.proxy.drain_timeout_secs);
        for b in retired {
            tokio::spawn(drain(b, grace));
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(changed),
        }
    }
}

/// Wait for a retired backend's in-flight requests and streams to finish
/// (up to `grace`), then let it end its upstream session.
pub async fn drain(b: Arc<dyn MediaBackend>, grace: Duration) {
    gauge!(names::SERVERS_DRAINING).increment(1.0);
    let t0 = Instant::now();
    let mut outcome = "drained";
    while b.in_flight() > 0 {
        if t0.elapsed() >= grace {
            outcome = "timeout";
            tracing::warn!(upstream = %b.name(), in_flight = b.in_flight(), "drain timeout; retiring with requests still open");
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    b.retire().await;
    histogram!(names::SERVER_DRAIN_DURATION).record(t0.elapsed().as_secs_f64());
    counter!(names::SERVERS_RETIRED, "outcome" => outcome).increment(1);
    gauge!(names::SERVERS_DRAINING).decrement(1.0);
    tracing::info!(upstream = %b.name(), outcome, "retired backend");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_append_only() {
        let r = ServerRegistry::new();
        assert_eq!(r.id_for("a"), ServerId(0));
        assert_eq!(r.id_for("b"), ServerId(1));
        assert_eq!(r.id_for("A"), ServerId(0));
        assert_eq!(
            r.peek_ids(["b", "c", "d"]),
            [ServerId(1), ServerId(2), ServerId(3)]
        );
        assert_eq!(r.len(), 2, "peek does not register");
        assert_eq!(r.id_for("c"), ServerId(2));
        assert_eq!(r.name(ServerId(1)).as_deref(), Some("b"));
    }
}
