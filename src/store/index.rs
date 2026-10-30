//! Persistence for the canonical catalog index ([`crate::index`]): the
//! entries last crawled from each server, the stable canonical id of every
//! grouped proxy id, and merged library views.

use super::StoreError;
use crate::index::entry::Entry;
use crate::types::{ProxyItemId, UpstreamItemId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Everything indexed from one server (by server *name*, like ids).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StoredServerIndex {
    pub server: String,
    /// Unix seconds of the crawl.
    pub synced_at: i64,
    pub ratings: HashMap<String, i64>,
    pub entries: Vec<Entry>,
}

/// A merged library view as persisted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredView {
    pub id: ProxyItemId,
    pub name: String,
    pub collection_type: Option<String>,
    /// (server name, upstream library id)
    pub members: Vec<(String, UpstreamItemId)>,
    pub template: Value,
}

pub trait IndexStore: Send + Sync {
    fn load_index(&self) -> Result<Vec<StoredServerIndex>, StoreError>;
    /// Replace everything stored for `idx.server`.
    fn save_server_index(&self, idx: &StoredServerIndex) -> Result<(), StoreError>;
    fn load_canonical(&self) -> Result<Vec<(ProxyItemId, ProxyItemId)>, StoreError>;
    /// Insert or update `member -> canonical` rows.
    fn save_canonical(&self, rows: &[(ProxyItemId, ProxyItemId)]) -> Result<(), StoreError>;
    fn load_views(&self) -> Result<Vec<StoredView>, StoreError>;
    /// Insert or replace a view (by id).
    fn save_view(&self, view: &StoredView) -> Result<(), StoreError>;
}

/// In-memory part of [`super::MemoryStore`].
#[derive(Default)]
pub(super) struct MemIndex {
    servers: HashMap<String, StoredServerIndex>,
    canonical: HashMap<ProxyItemId, ProxyItemId>,
    views: Vec<StoredView>,
}

impl MemIndex {
    pub(super) fn load_index(&self) -> Vec<StoredServerIndex> {
        let mut v: Vec<_> = self.servers.values().cloned().collect();
        v.sort_by(|a, b| a.server.cmp(&b.server));
        v
    }
    pub(super) fn save_server_index(&mut self, idx: &StoredServerIndex) {
        self.servers.insert(idx.server.clone(), idx.clone());
    }
    pub(super) fn load_canonical(&self) -> Vec<(ProxyItemId, ProxyItemId)> {
        self.canonical
            .iter()
            .map(|(a, b)| (a.clone(), b.clone()))
            .collect()
    }
    pub(super) fn save_canonical(&mut self, rows: &[(ProxyItemId, ProxyItemId)]) {
        for (m, c) in rows {
            self.canonical.insert(m.clone(), c.clone());
        }
    }
    pub(super) fn load_views(&self) -> Vec<StoredView> {
        self.views.clone()
    }
    pub(super) fn save_view(&mut self, view: &StoredView) {
        self.views.retain(|v| v.id != view.id);
        self.views.push(view.clone());
    }
}
