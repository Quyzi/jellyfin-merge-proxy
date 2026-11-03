//! Persistence for state the proxy owns outright instead of forwarding to an
//! upstream: per-user settings, display preferences and playlists
//! ([`crate::local`]).

use super::StoreError;
use serde_json::Value;
use std::collections::BTreeMap;

pub trait LocalStore: Send + Sync {
    /// `(user_id, UserConfiguration)`.
    fn load_settings(&self) -> Result<Vec<(String, Value)>, StoreError>;
    fn put_settings(&self, user: &str, v: &Value) -> Result<(), StoreError>;
    /// `(user_id, prefs_guid_n, client, record)`.
    fn load_display_prefs(&self) -> Result<Vec<(String, String, String, Value)>, StoreError>;
    fn put_display_prefs(
        &self,
        user: &str,
        prefs: &str,
        client: &str,
        v: &Value,
    ) -> Result<(), StoreError>;
    /// Playlist records as stored JSON.
    fn load_playlists(&self) -> Result<Vec<Value>, StoreError>;
    fn put_playlist(&self, id: &str, owner: &str, v: &Value) -> Result<(), StoreError>;
    fn delete_playlist(&self, id: &str) -> Result<(), StoreError>;
}

/// In-memory part of [`super::MemoryStore`].
#[derive(Default)]
pub(super) struct MemLocal {
    settings: BTreeMap<String, Value>,
    prefs: BTreeMap<(String, String, String), Value>,
    playlists: BTreeMap<String, Value>,
}

impl MemLocal {
    pub(super) fn settings(&self) -> Vec<(String, Value)> {
        self.settings
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
    pub(super) fn put_settings(&mut self, user: &str, v: &Value) {
        self.settings.insert(user.to_string(), v.clone());
    }
    pub(super) fn prefs(&self) -> Vec<(String, String, String, Value)> {
        self.prefs
            .iter()
            .map(|((u, p, c), v)| (u.clone(), p.clone(), c.clone(), v.clone()))
            .collect()
    }
    pub(super) fn put_prefs(&mut self, user: &str, prefs: &str, client: &str, v: &Value) {
        self.prefs.insert(
            (user.to_string(), prefs.to_string(), client.to_string()),
            v.clone(),
        );
    }
    pub(super) fn playlists(&self) -> Vec<Value> {
        self.playlists.values().cloned().collect()
    }
    pub(super) fn put_playlist(&mut self, id: &str, v: &Value) {
        self.playlists.insert(id.to_string(), v.clone());
    }
    pub(super) fn delete_playlist(&mut self, id: &str) {
        self.playlists.remove(id);
    }
}
