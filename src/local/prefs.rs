//! Persisted `DisplayPreferences`, per (proxy user, preferences id, client),
//! following Jellyfin's `DisplayPreferencesController`. Records are stored
//! as JSON through [`crate::store::LocalStore`].

mod http;

pub use http::handle;

use crate::store::Store;
use crate::types::normalize_hex_id;
use md5::{Digest, Md5};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

type Key = (String, String, String);

pub struct DisplayPrefs {
    store: Arc<dyn Store>,
    map: RwLock<HashMap<Key, Value>>,
}

/// The entity defaults (`DisplayPreferences.cs`, `ItemDisplayPreferences.cs`).
pub fn default_record() -> Value {
    json!({
        "ShowBackdrop": true,
        "ShowSidebar": false,
        "ScrollDirection": "Horizontal",
        "ChromecastVersion": "Stable",
        "EnableNextVideoInfoOverlay": false,
        "SkipBackwardLength": 10000,
        "SkipForwardLength": 30000,
        "HomeSections": [],
        "SortBy": "SortName",
        "SortOrder": "Ascending",
        "RememberIndexing": false,
        "RememberSorting": false,
        "Custom": {}
    })
}

/// 32-hex form of the preferences guid: a hex id as written, else the MD5
/// of the UTF-16LE text read as a .NET `Guid` (`BaseExtensions.GetMD5`).
pub fn prefs_guid(raw: &str) -> String {
    if let Some(n) = normalize_hex_id(raw) {
        return n;
    }
    let bytes: Vec<u8> = raw.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let d = Md5::digest(&bytes);
    // Guid(byte[]) is little-endian in its first three fields.
    let order = [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15];
    order.iter().map(|&i| format!("{:02x}", d[i])).collect()
}

/// Lowercase "D" form of a 32-hex guid.
pub fn dashed(n: &str) -> String {
    format!(
        "{}-{}-{}-{}-{}",
        &n[0..8],
        &n[8..12],
        &n[12..16],
        &n[16..20],
        &n[20..32]
    )
}

impl DisplayPrefs {
    pub fn load(store: Arc<dyn Store>) -> Self {
        let map = match store.load_display_prefs() {
            Ok(rows) => rows
                .into_iter()
                .map(|(u, p, c, v)| ((u, p, c), v))
                .collect(),
            Err(e) => {
                tracing::warn!("loading display preferences: {e}");
                HashMap::new()
            }
        };
        Self {
            store,
            map: RwLock::new(map),
        }
    }

    pub fn len(&self) -> usize {
        self.map.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The stored record; the client is matched case-sensitively.
    pub fn get(&self, user: &str, prefs: &str, client: &str) -> Option<Value> {
        let k = (user.to_string(), prefs.to_string(), client.to_string());
        self.map.read().unwrap().get(&k).cloned()
    }

    pub fn put(&self, user: &str, prefs: &str, client: &str, v: Value) {
        if let Err(e) = self.store.put_display_prefs(user, prefs, client, &v) {
            tracing::warn!("persisting display preferences: {e}");
        }
        let k = (user.to_string(), prefs.to_string(), client.to_string());
        self.map.write().unwrap().insert(k, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_guid_ids_hash_like_dotnet() {
        assert_eq!(
            dashed(&prefs_guid("usersettings")),
            "3ce5b65d-e116-d731-65d1-efc4a30ec35c"
        );
        assert_eq!(
            dashed(&prefs_guid("home")),
            "d807cd28-a0ec-0943-8479-27aac34d1c56"
        );
    }

    #[test]
    fn guid_ids_keep_their_written_order() {
        let g = "3CE5B65D-E116-D731-65D1-EFC4A30EC35C";
        assert_eq!(prefs_guid(g), "3ce5b65de116d73165d1efc4a30ec35c");
    }
}
