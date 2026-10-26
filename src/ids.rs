//! Mapping between upstream ids and proxy ids.
//!
//! [`IdMapper`] is the seam; [`HashedIdTable`] is the default implementation:
//! a proxy id is `sha256("jmp1:" + server_name + ":" + upstream_id)[..16]`
//! rendered as 32 lowercase hex characters, so it looks exactly like a
//! Jellyfin GUID. Because a hash cannot be inverted, every id handed to a
//! client is recorded in an in-memory table which is also appended to
//! `<data_dir>/ids.tsv`, so ids stay valid across proxy restarts.
//! [`MeteredIdMapper`] adds the `jmp_ids_*` metrics to any mapper.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, RwLock};

use crate::servers::ServerRegistry;
use crate::store::{IdRow, IdStore, StoreError};
use crate::telemetry::names;
pub use crate::types::normalize_hex_id;
use crate::types::{ProxyItemId, ServerId, UpstreamItemId};
use metrics::{counter, gauge};

/// A library view that combines libraries from several upstreams.
#[derive(Debug, Clone)]
pub struct MergedView {
    pub id: ProxyItemId,
    pub name: String,
    pub collection_type: Option<String>,
    /// (server, upstream library id)
    pub members: Vec<(ServerId, UpstreamItemId)>,
    /// The view JSON (from the first member) already rewritten for clients.
    pub template: Value,
}

/// What a proxy id points at.
#[derive(Debug, Clone)]
pub enum Target {
    Item {
        upstream: ServerId,
        id: UpstreamItemId,
    },
    View(Arc<MergedView>),
}

/// Result of [`IdMapper::encode`].
#[derive(Debug, Clone)]
pub struct Encoded {
    pub id: ProxyItemId,
    /// The mapping did not exist before this call.
    pub new: bool,
}

/// Translates ids between upstream servers and the merged proxy view.
pub trait IdMapper: Send + Sync {
    /// Map an upstream id to its proxy id, registering it.
    fn encode(&self, server: ServerId, id: &UpstreamItemId) -> Encoded;
    /// Look a proxy id up.
    fn decode(&self, id: &ProxyItemId) -> Option<Target>;
    /// Register a merged library view under its (precomputed) id.
    fn register_view(&self, view: MergedView) -> Arc<MergedView>;
    /// Number of item mappings for one server.
    fn size_of(&self, server: ServerId) -> u64;
    /// (server name, mappings) for every server.
    fn sizes(&self) -> Vec<(String, u64)>;
    /// Total table entries (items and views).
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Publish periodic gauges (no-op unless decorated).
    fn publish_gauges(&self) {}

    // ---- string conveniences used by the rewriters ----

    /// Map a raw upstream id string; non-GUID strings pass through.
    fn to_proxy(&self, server: ServerId, raw: &str) -> String {
        match UpstreamItemId::parse(raw) {
            Some(u) => self.encode(server, &u).id.into_string(),
            None => raw.to_string(),
        }
    }
    /// Resolve a raw proxy id string.
    fn resolve(&self, raw: &str) -> Option<Target> {
        self.decode(&ProxyItemId::parse(raw)?)
    }
    /// Resolve to an upstream item id on `server` (merged views resolve to
    /// their member on that server, if any).
    fn resolve_for(&self, raw: &str, server: ServerId) -> Option<UpstreamItemId> {
        match self.resolve(raw)? {
            Target::Item { upstream, id } if upstream == server => Some(id),
            Target::Item { .. } => None,
            Target::View(v) => v
                .members
                .iter()
                .find(|(u, _)| *u == server)
                .map(|(_, id)| id.clone()),
        }
    }
}

pub fn looks_like_id(s: &str) -> bool {
    normalize_hex_id(s).is_some()
}

/// Hash arbitrary parts into a 32-hex id.
pub fn hash_id(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    h.update(b"jmp1");
    for p in parts {
        h.update(b":");
        h.update(p.as_bytes());
    }
    hex::encode(&h.finalize()[..16])
}

/// Proxy id for a merged view (or any synthetic object) from its parts.
pub fn synthetic_id(parts: &[&str]) -> ProxyItemId {
    ProxyItemId::parse(&hash_id(parts)).expect("hash_id yields 32 hex chars")
}

/// Proxy id of `id` on the server called `server_name`
/// (`sha256("jmp1:" + lowercase name + ":" + id)[..16]`).
pub fn item_proxy_id(server_name: &str, id: &UpstreamItemId) -> ProxyItemId {
    synthetic_id(&[&server_name.to_ascii_lowercase(), id.as_str()])
}

/// Default [`IdMapper`]: deterministic hash ids plus a lookup table,
/// optionally persisted to `ids.tsv`.
///
/// Proxy ids hash the server *name* (not its URL), so moving a server to a
/// new URL keeps every id valid. Entries for servers that were removed from
/// the config are kept: they still decode to that server's [`ServerId`],
/// which the proxy answers with 410 Gone.
pub struct HashedIdTable {
    servers: Arc<ServerRegistry>,
    table: RwLock<HashMap<ProxyItemId, Target>>,
    writer: Option<Writer>,
    per_server: RwLock<HashMap<ServerId, AtomicU64>>,
}

/// Backwards-compatible name.
pub type IdMap = HashedIdTable;

/// Background writer: new mappings go through a channel to a thread that
/// saves them in batches.
struct Writer {
    tx: std::sync::Mutex<mpsc::Sender<IdRow>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl HashedIdTable {
    pub fn new(servers: Arc<ServerRegistry>) -> Self {
        HashedIdTable {
            servers,
            table: RwLock::new(HashMap::new()),
            writer: None,
            per_server: RwLock::new(HashMap::new()),
        }
    }

    fn bump(&self, server: ServerId) {
        if let Some(n) = self.per_server.read().unwrap().get(&server) {
            n.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.per_server
            .write()
            .unwrap()
            .entry(server)
            .or_default()
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Load every mapping from `store` and write new ones through to it.
    /// Writes are batched on a background thread, so [`IdMapper::encode`]
    /// never waits for the disk; dropping the table flushes them.
    pub fn with_store(mut self, store: Arc<dyn IdStore>) -> Result<Self, StoreError> {
        let rows = store.load_ids()?;
        let loaded = rows.len();
        {
            let mut table = self.table.write().unwrap();
            for r in rows {
                // Servers no longer configured are registered too, so
                // their old ids answer 410 Gone (or work again if re-added).
                let idx = self.servers.id_for(&r.server);
                let target = Target::Item {
                    upstream: idx,
                    id: r.upstream_id,
                };
                if table.insert(r.proxy_id, target).is_none() {
                    self.bump(idx);
                }
            }
        }
        tracing::info!("loaded {loaded} id mappings from the state store");
        counter!(names::PERSIST_OPS, "store" => "ids", "op" => "load", "outcome" => "success")
            .increment(1);
        let (tx, rx) = mpsc::channel::<IdRow>();
        let handle = std::thread::Builder::new()
            .name("id-persist".into())
            .spawn(move || {
                while let Ok(first) = rx.recv() {
                    let mut batch = vec![first];
                    batch.extend(rx.try_iter());
                    let outcome = match store.append_ids(&batch) {
                        Ok(()) => "success",
                        Err(e) => {
                            tracing::warn!("saving {} id mappings failed: {e}", batch.len());
                            "error"
                        }
                    };
                    counter!(names::PERSIST_OPS, "store" => "ids", "op" => "save", "outcome" => outcome).increment(1);
                }
            })
            .map_err(|e| StoreError::Invalid(format!("starting the id writer: {e}")))?;
        self.writer = Some(Writer {
            tx: std::sync::Mutex::new(tx),
            handle: Some(handle),
        });
        Ok(self)
    }

    fn compute(&self, upstream: ServerId, id: &UpstreamItemId) -> ProxyItemId {
        item_proxy_id(&self.servers.name(upstream).unwrap_or_default(), id)
    }
}

impl Drop for HashedIdTable {
    /// Wait for queued mappings to be saved.
    fn drop(&mut self) {
        if let Some(mut w) = self.writer.take() {
            drop(w.tx);
            if let Some(h) = w.handle.take() {
                let _ = h.join();
            }
        }
    }
}

impl IdMapper for HashedIdTable {
    fn encode(&self, server: ServerId, id: &UpstreamItemId) -> Encoded {
        let pid = self.compute(server, id);
        if self.table.read().unwrap().contains_key(&pid) {
            return Encoded {
                id: pid,
                new: false,
            };
        }
        let mut t = self.table.write().unwrap();
        if t.contains_key(&pid) {
            return Encoded {
                id: pid,
                new: false,
            };
        }
        t.insert(
            pid.clone(),
            Target::Item {
                upstream: server,
                id: id.clone(),
            },
        );
        drop(t);
        self.bump(server);
        if let (Some(w), Some(name)) = (&self.writer, self.servers.name(server)) {
            let _ = w.tx.lock().unwrap().send(IdRow {
                proxy_id: pid.clone(),
                server: name,
                upstream_id: id.clone(),
            });
        }
        Encoded { id: pid, new: true }
    }

    fn decode(&self, id: &ProxyItemId) -> Option<Target> {
        self.table.read().unwrap().get(id).cloned()
    }

    fn register_view(&self, view: MergedView) -> Arc<MergedView> {
        let v = Arc::new(view);
        self.table
            .write()
            .unwrap()
            .insert(v.id.clone(), Target::View(v.clone()));
        v
    }

    fn size_of(&self, server: ServerId) -> u64 {
        self.per_server
            .read()
            .unwrap()
            .get(&server)
            .map(|n| n.load(Ordering::Relaxed))
            .unwrap_or(0)
    }

    fn sizes(&self) -> Vec<(String, u64)> {
        self.servers
            .names()
            .into_iter()
            .enumerate()
            .map(|(i, n)| (n, self.size_of(ServerId(i))))
            .collect()
    }

    fn len(&self) -> usize {
        self.table.read().unwrap().len()
    }
}

mod metered;
pub use metered::MeteredIdMapper;

#[cfg(test)]
mod tests;
