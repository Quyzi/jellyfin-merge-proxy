//! [`IndexService`]: keeps the catalog index fresh and hands out
//! snapshots.
//!
//! On start the last crawl of every server is loaded from the store, so the
//! index is warm immediately. A background task re-crawls every server each
//! `[index] refresh_interval_secs`, and right away when a server appears
//! that has never been crawled (config reload). A server that is down keeps
//! its last crawl; queries skip its copies while it is down.

use super::crawl::{Crawler, ItemsCrawler};
use super::grouping::ProviderGrouping;
use super::groups::CanonicalGroups;
use super::rank::{ranker, ServerRank};
use super::snapshot::{ServerEntries, Snapshot};
use crate::backend::MediaBackend;
use crate::config::Config;
use crate::ids::IdMapper;
use crate::servers::ServerRegistry;
use crate::store::{Store, StoredServerIndex};
use crate::telemetry::names;
use crate::types::{ProxyItemId, ServerId};
use arc_swap::ArcSwapOption;
use metrics::{counter, gauge, histogram};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

/// What a snapshot was built from; a change triggers a rebuild.
#[derive(Debug, Clone, PartialEq)]
struct BuildKey {
    /// (server name, priority, crawl time) in config order.
    servers: Vec<(String, i64, i64)>,
    dedupe: bool,
    types: Vec<String>,
    prefer: &'static str,
}

pub struct IndexService {
    store: Arc<dyn Store>,
    registry: Arc<ServerRegistry>,
    groups: Arc<CanonicalGroups>,
    crawler: Box<dyn Crawler>,
    /// Last crawl per server name.
    crawled: Mutex<BTreeMap<String, Arc<StoredServerIndex>>>,
    persisted: Mutex<HashMap<ProxyItemId, ProxyItemId>>,
    snap: ArcSwapOption<Snapshot>,
    /// `(previous, new)` after every rebuild.
    published: broadcast::Sender<(Option<Arc<Snapshot>>, Arc<Snapshot>)>,
    built: Mutex<Option<BuildKey>>,
    refreshing: tokio::sync::Mutex<()>,
    wake: tokio::sync::Notify,
    /// Live servers whose catalog the index does not hold yet (newly added,
    /// down at start, or failing to crawl). Their items cannot be rated.
    uncovered: Mutex<std::collections::HashSet<ServerId>>,
}

impl IndexService {
    pub fn new(
        store: Arc<dyn Store>,
        registry: Arc<ServerRegistry>,
        groups: Arc<CanonicalGroups>,
        page_size: usize,
    ) -> Self {
        Self::with_crawler(
            store,
            registry,
            groups,
            Box::new(ItemsCrawler { page_size }),
        )
    }

    pub fn with_crawler(
        store: Arc<dyn Store>,
        registry: Arc<ServerRegistry>,
        groups: Arc<CanonicalGroups>,
        crawler: Box<dyn Crawler>,
    ) -> Self {
        let mut crawled = BTreeMap::new();
        match store.load_index() {
            Ok(list) => {
                for s in list {
                    crawled.insert(s.server.clone(), Arc::new(s));
                }
            }
            Err(e) => tracing::warn!("loading the catalog index failed: {e}"),
        }
        let persisted = store
            .load_canonical()
            .map(|r| r.into_iter().collect())
            .unwrap_or_else(|e| {
                tracing::warn!("loading canonical ids failed: {e}");
                HashMap::new()
            });
        IndexService {
            store,
            registry,
            groups,
            crawler,
            crawled: Mutex::new(crawled),
            persisted: Mutex::new(persisted),
            snap: ArcSwapOption::empty(),
            published: broadcast::channel(4).0,
            built: Mutex::new(None),
            refreshing: tokio::sync::Mutex::new(()),
            wake: tokio::sync::Notify::new(),
            uncovered: Mutex::new(Default::default()),
        }
    }

    /// The current snapshot, if the index is enabled and covers every
    /// server that is up.
    pub fn snapshot(&self, cfg: &Config, set: &[Arc<dyn MediaBackend>]) -> Option<Arc<Snapshot>> {
        if !cfg.index.enabled {
            return None;
        }
        let built = self.built.lock().unwrap();
        let covered = |n: &str| {
            built
                .as_ref()
                .is_some_and(|k| k.servers.iter().any(|s| s.0 == n))
        };
        if set.iter().any(|b| !b.is_down() && !covered(b.name())) {
            return None;
        }
        drop(built);
        self.snap.load_full()
    }

    /// The group (copies on several servers) `id` belongs to, if any.
    /// The last built snapshot, warm or not (per-item lookups that do not
    /// need every server covered).
    pub fn latest(&self) -> Option<Arc<Snapshot>> {
        self.snap.load_full()
    }

    /// Every rebuild: the snapshot it replaced (if any) and the new one.
    pub fn subscribe(&self) -> broadcast::Receiver<(Option<Arc<Snapshot>>, Arc<Snapshot>)> {
        self.published.subscribe()
    }

    pub fn group(&self, id: &ProxyItemId) -> Option<Arc<super::groups::Group>> {
        use super::groups::GroupLookup;
        self.groups.group(id)
    }

    /// Ask the background task to re-crawl now.
    pub fn poke(&self) {
        self.wake.notify_one();
    }

    fn key(&self, cfg: &Config, set: &[Arc<dyn MediaBackend>]) -> BuildKey {
        let crawled = self.crawled.lock().unwrap();
        BuildKey {
            servers: set
                .iter()
                .filter_map(|b| {
                    let c = crawled.get(b.name())?;
                    Some((b.name().to_string(), b.priority(), c.synced_at))
                })
                .collect(),
            dedupe: cfg.merge.dedupe,
            types: cfg.merge.dedupe_types.clone(),
            prefer: cfg.index.prefer.as_str(),
        }
    }

    /// Rebuild the snapshot if its inputs changed.
    pub fn rebuild(&self, cfg: &Config, set: &[Arc<dyn MediaBackend>], ids: &dyn IdMapper) {
        let key = self.key(cfg, set);
        if self.built.lock().unwrap().as_ref() == Some(&key) {
            return;
        }
        let t = Instant::now();
        let crawled = self.crawled.lock().unwrap().clone();
        let servers: Vec<Arc<ServerEntries>> = set
            .iter()
            .filter_map(|b| {
                let c = crawled.get(b.name())?;
                Some(Arc::new(ServerEntries {
                    server: b.id(),
                    entries: c.entries.clone(),
                    ratings: c.ratings.clone(),
                }))
            })
            .collect();
        if servers.is_empty() {
            // Nothing crawled yet (or every server unreachable since start):
            // an empty snapshot would look like a built index and let
            // parental checks pass unknown items.
            return;
        }
        let ranks: HashMap<ServerId, ServerRank> = set
            .iter()
            .enumerate()
            .map(|(order, b)| {
                let r = ServerRank {
                    priority: b.priority(),
                    order,
                };
                (b.id(), r)
            })
            .collect();
        let policy = ProviderGrouping {
            enabled: cfg.merge.dedupe,
            types: cfg.merge.dedupe_types.clone(),
        };
        let rk = ranker(cfg.index.prefer.as_str());
        let persisted = self.persisted.lock().unwrap().clone();
        let (snap, out) = Snapshot::build(
            &servers,
            &policy,
            rk.as_ref(),
            &|s| ranks.get(&s).copied().unwrap_or_default(),
            &|s, u| ids.encode(s, u).id,
            &persisted,
        );
        if !out.canonical_rows.is_empty() {
            let outcome = match self.store.save_canonical(&out.canonical_rows) {
                Ok(()) => "success",
                Err(e) => {
                    tracing::warn!("saving canonical ids failed: {e}");
                    "error"
                }
            };
            counter!(names::PERSIST_OPS, "store" => "canonical", "op" => "save", "outcome" => outcome)
                .increment(1);
            self.persisted
                .lock()
                .unwrap()
                .extend(out.canonical_rows.iter().cloned());
        }
        gauge!(names::INDEX_CANONICAL, "kind" => "items").set(snap.items.len() as f64);
        gauge!(names::INDEX_CANONICAL, "kind" => "merged").set(out.merged_groups as f64);
        gauge!(names::INDEX_CANONICAL, "kind" => "names").set(snap.names.len() as f64);
        histogram!(names::INDEX_BUILD_DURATION).record(t.elapsed().as_secs_f64());
        tracing::info!(
            items = snap.items.len(),
            merged = out.merged_groups,
            names = snap.names.len(),
            servers = servers.len(),
            elapsed_ms = t.elapsed().as_millis() as u64,
            "catalog index rebuilt"
        );
        self.groups.publish(snap.groups.clone());
        let new = Arc::new(snap);
        let old = self.snap.swap(Some(new.clone()));
        let _ = self.published.send((old, new));
        *self.built.lock().unwrap() = Some(key);
    }

    /// Crawl every live server in `set` (concurrently) and store results.
    pub async fn crawl_all(&self, set: &[Arc<dyn MediaBackend>]) {
        let _g = self.refreshing.lock().await;
        let jobs = set.iter().map(|b| self.crawl_one(b));
        futures_util::future::join_all(jobs).await;
    }

    async fn crawl_one(&self, b: &Arc<dyn MediaBackend>) {
        let name = b.name().to_string();
        if b.is_down() {
            counter!(names::INDEX_SYNCS, "server" => name, "outcome" => "skipped").increment(1);
            return;
        }
        let t = Instant::now();
        let res = self.crawler.crawl(b).await;
        let secs = t.elapsed().as_secs_f64();
        histogram!(names::INDEX_SYNC_DURATION, "server" => name.clone()).record(secs);
        let c = match res {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(upstream = %name, "index crawl failed: {e}");
                counter!(names::INDEX_SYNCS, "server" => name, "outcome" => "error").increment(1);
                return;
            }
        };
        let now = crate::telemetry::unix_now() as i64;
        let synced_at = {
            // Strictly increasing, so a rebuild always notices a new crawl.
            let prev = self.crawled.lock().unwrap().get(&name).map(|p| p.synced_at);
            prev.map_or(now, |p| now.max(p + 1))
        };
        let idx = StoredServerIndex {
            server: name.clone(),
            synced_at,
            ratings: c.ratings,
            entries: c.entries,
        };
        let outcome = match self.store.save_server_index(&idx) {
            Ok(()) => "success",
            Err(e) => {
                tracing::warn!(upstream = %name, "saving the index failed: {e}");
                "error"
            }
        };
        counter!(names::PERSIST_OPS, "store" => "index", "op" => "save", "outcome" => outcome)
            .increment(1);
        tracing::info!(
            upstream = %name,
            items = idx.entries.len(),
            libraries = c.libraries,
            elapsed_ms = (secs * 1000.0) as u64,
            "index crawl finished"
        );
        gauge!(names::INDEX_ITEMS, "server" => name.clone()).set(idx.entries.len() as f64);
        gauge!(names::INDEX_LAST_SYNC, "server" => name.clone()).set(now as f64);
        counter!(names::INDEX_SYNCS, "server" => name.clone(), "outcome" => "success").increment(1);
        self.crawled.lock().unwrap().insert(name, Arc::new(idx));
    }

    /// Servers in `set` that are up but were never crawled.
    fn missing(&self, set: &[Arc<dyn MediaBackend>]) -> bool {
        let crawled = self.crawled.lock().unwrap();
        set.iter()
            .any(|b| !b.is_down() && !crawled.contains_key(b.name()))
    }

    /// Record which live servers in `set` the index does not cover.
    pub fn note_coverage(&self, set: &[Arc<dyn MediaBackend>]) {
        let missing: std::collections::HashSet<ServerId> = {
            let crawled = self.crawled.lock().unwrap();
            set.iter()
                .filter(|b| !b.is_down() && !crawled.contains_key(b.name()))
                .map(|b| b.id())
                .collect()
        };
        *self.uncovered.lock().unwrap() = missing;
    }

    /// Whether `server` is live but not in the index (see [`Self::note_coverage`]).
    pub fn is_uncovered(&self, server: ServerId) -> bool {
        self.uncovered.lock().unwrap().contains(&server)
    }

    /// Background loop: crawl when due or when a new server shows up,
    /// rebuild when inputs change.
    pub fn spawn(self: &Arc<Self>, weak: std::sync::Weak<crate::AppState>) {
        let me = self.clone();
        tokio::spawn(async move {
            let mut last: Option<Instant> = None;
            let mut last_try: Option<Instant> = None;
            loop {
                let Some(st) = weak.upgrade() else { return };
                let cfg = st.cfg();
                let set = st.backends();
                if cfg.index.enabled {
                    let every = Duration::from_secs(cfg.index.refresh_interval_secs);
                    let due = last.is_none_or(|l| l.elapsed() >= every);
                    // Retry a server that never crawled, but not in a tight loop.
                    let retry = me.missing(&set[..])
                        && last_try.is_none_or(|l| l.elapsed() >= Duration::from_secs(30));
                    if due || retry {
                        me.crawl_all(&set[..]).await;
                        last_try = Some(Instant::now());
                        if due {
                            last = last_try;
                        }
                    }
                    me.note_coverage(&set[..]);
                    me.rebuild(&cfg, &set[..], st.ids.as_ref());
                }
                drop((st, set));
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                    _ = me.wake.notified() => { last = None; last_try = None; }
                }
            }
        });
    }

    /// Server ids registered for crawled server names (for tests/debug).
    pub fn crawled_servers(&self) -> Vec<(String, Option<ServerId>, usize)> {
        self.crawled
            .lock()
            .unwrap()
            .values()
            .map(|c| {
                (
                    c.server.clone(),
                    self.registry.lookup(&c.server),
                    c.entries.len(),
                )
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
