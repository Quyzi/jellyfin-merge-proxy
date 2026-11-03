//! Per-proxy-user `UserConfiguration`, persisted in the proxy's own database.
//! `put` replaces the whole configuration the way Jellyfin's
//! `UserManager.UpdateConfigurationAsync` does.

use crate::store::Store;
use crate::types::normalize_hex_id;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

const CAST_RECEIVER_ID: &str = "F007D354";
const SUBTITLE_MODES: &[&str] = &["Default", "Always", "OnlyForced", "None", "Smart"];
const BOOLS: &[&str] = &[
    "PlayDefaultAudioTrack",
    "DisplayMissingEpisodes",
    "DisplayCollectionsView",
    "EnableLocalPassword",
    "HidePlayedInLatest",
    "RememberAudioSelections",
    "RememberSubtitleSelections",
    "EnableNextEpisodeAutoPlay",
];
const GUID_LISTS: &[&str] = &[
    "GroupedFolders",
    "OrderedViews",
    "LatestItemsExcludes",
    "MyMediaExcludes",
];

pub struct UserSettings {
    store: Arc<dyn Store>,
    map: RwLock<HashMap<String, Value>>,
}

/// A fresh user's configuration (Jellyfin's `UserConfiguration` defaults).
pub fn defaults() -> Value {
    json!({
        "PlayDefaultAudioTrack": true,
        "SubtitleLanguagePreference": "",
        "DisplayMissingEpisodes": false,
        "GroupedFolders": [],
        "SubtitleMode": "Default",
        "DisplayCollectionsView": false,
        "EnableLocalPassword": false,
        "OrderedViews": [],
        "LatestItemsExcludes": [],
        "MyMediaExcludes": [],
        "HidePlayedInLatest": true,
        "RememberAudioSelections": true,
        "RememberSubtitleSelections": true,
        "EnableNextEpisodeAutoPlay": true,
        "CastReceiverId": CAST_RECEIVER_ID
    })
}

fn body_key<'a>(body: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    body.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
}

impl UserSettings {
    pub fn load(store: Arc<dyn Store>) -> Self {
        let map = match store.load_settings() {
            Ok(rows) => rows.into_iter().collect(),
            Err(e) => {
                tracing::warn!("loading user settings: {e}");
                HashMap::new()
            }
        };
        Self {
            store,
            map: RwLock::new(map),
        }
    }

    pub fn get(&self, user: &str) -> Value {
        self.map
            .read()
            .unwrap()
            .get(user)
            .cloned()
            .unwrap_or_else(defaults)
    }

    /// Replace `user`'s configuration with `body` over the defaults.
    /// `Err` carries a reason for a 400.
    pub fn put(&self, user: &str, body: &Value) -> Result<(), String> {
        let body = body.as_object().ok_or("configuration must be an object")?;
        let mut out = defaults();
        let o = out.as_object_mut().unwrap();
        for key in BOOLS {
            match body_key(body, key) {
                None | Some(Value::Null) => {}
                Some(v @ Value::Bool(_)) => {
                    o.insert((*key).into(), v.clone());
                }
                Some(_) => return Err(format!("{key} must be a boolean")),
            }
        }
        match body_key(body, "SubtitleMode") {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) => {
                let mode = SUBTITLE_MODES
                    .iter()
                    .find(|m| m.eq_ignore_ascii_case(s))
                    .ok_or("invalid SubtitleMode")?;
                o.insert("SubtitleMode".into(), json!(mode));
            }
            Some(_) => return Err("SubtitleMode must be a string".into()),
        }
        match body_key(body, "SubtitleLanguagePreference") {
            None | Some(Value::Null) => {}
            Some(v @ Value::String(_)) => {
                o.insert("SubtitleLanguagePreference".into(), v.clone());
            }
            Some(_) => return Err("SubtitleLanguagePreference must be a string".into()),
        }
        for key in GUID_LISTS {
            match body_key(body, key) {
                None | Some(Value::Null) => {}
                Some(Value::Array(a)) => {
                    let ids: Vec<Value> = a
                        .iter()
                        .filter_map(|v| v.as_str().and_then(normalize_hex_id))
                        .map(Value::String)
                        .collect();
                    o.insert((*key).into(), Value::Array(ids));
                }
                Some(_) => return Err(format!("{key} must be an array")),
            }
        }
        if let Some(prev) = self.map.read().unwrap().get(user) {
            if let Some(c) = prev.get("CastReceiverId") {
                o.insert("CastReceiverId".into(), c.clone());
            }
        }
        if let Some(Value::String(s)) = body_key(body, "CastReceiverId") {
            if s == CAST_RECEIVER_ID {
                o.insert("CastReceiverId".into(), json!(s));
            }
        }
        match body_key(body, "AudioLanguagePreference") {
            None | Some(Value::Null) => {}
            Some(Value::String(s)) => {
                let mut first = Map::new();
                first.insert("AudioLanguagePreference".into(), json!(s));
                first.extend(std::mem::take(o));
                *o = first;
            }
            Some(_) => return Err("AudioLanguagePreference must be a string".into()),
        }
        if let Err(e) = self.store.put_settings(user, &out) {
            tracing::warn!("persisting user settings: {e}");
        }
        self.map.write().unwrap().insert(user.to_string(), out);
        Ok(())
    }

    fn flag(&self, user: &str, key: &str) -> bool {
        self.get(user)
            .get(key)
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    fn ids(&self, user: &str, key: &str) -> Vec<String> {
        self.get(user)
            .get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn display_missing_episodes(&self, user: &str) -> bool {
        self.flag(user, "DisplayMissingEpisodes")
    }

    pub fn enable_next_episode_auto_play(&self, user: &str) -> bool {
        self.flag(user, "EnableNextEpisodeAutoPlay")
    }

    pub fn ordered_views(&self, user: &str) -> Vec<String> {
        self.ids(user, "OrderedViews")
    }

    pub fn my_media_excludes(&self, user: &str) -> Vec<String> {
        self.ids(user, "MyMediaExcludes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;

    fn settings() -> UserSettings {
        UserSettings::load(Arc::new(MemoryStore::new()))
    }

    #[test]
    fn put_replaces_the_whole_configuration() {
        let s = settings();
        let id = "0123456789abcdef0123456789abcdef";
        s.put(
            "u",
            &json!({"displaymissingepisodes": true, "OrderedViews": [id, "junk"]}),
        )
        .unwrap();
        assert!(s.display_missing_episodes("u"));
        assert_eq!(s.ordered_views("u"), [id]);
        s.put("u", &json!({"HidePlayedInLatest": false})).unwrap();
        assert!(!s.display_missing_episodes("u"), "unsent keys reset");
        assert!(s.ordered_views("u").is_empty());
        assert_eq!(s.get("u")["HidePlayedInLatest"], false);
    }

    #[test]
    fn audio_preference_comes_first_and_bad_bodies_are_refused() {
        let s = settings();
        s.put("u", &json!({"AudioLanguagePreference": "eng"}))
            .unwrap();
        let v = s.get("u");
        assert_eq!(
            v.as_object().unwrap().keys().next().unwrap(),
            "AudioLanguagePreference"
        );
        assert!(s.put("u", &json!([1])).is_err());
        assert!(s.put("u", &json!({"SubtitleMode": "Bogus"})).is_err());
        assert_eq!(s.get("u"), v, "a refused body changes nothing");
    }
}
