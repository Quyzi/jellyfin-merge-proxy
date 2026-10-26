//! Client-facing authentication: token extraction and session store.

use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::RwLock;

#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    pub token: String,
    pub session_id: String,
    pub user_name: String,
    pub user_id: String,
    pub client: String,
    pub device_name: String,
    pub device_id: String,
    pub app_version: String,
    pub created: String,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("token", &"***")
            .field("session_id", &self.session_id)
            .field("user_name", &self.user_name)
            .field("client", &self.client)
            .field("device_name", &self.device_name)
            .field("app_version", &self.app_version)
            .field("created", &self.created)
            .finish_non_exhaustive()
    }
}

/// Session as exposed on the debug endpoint: no token, only a short prefix
/// of its hash for correlation.
#[derive(Debug, Clone, Serialize)]
pub struct RedactedSession {
    pub token_fingerprint: String,
    pub user_name: String,
    pub client: String,
    pub device_name: String,
    pub app_version: String,
    pub created: String,
}

impl Session {
    pub fn redacted(&self) -> RedactedSession {
        use sha2::Digest;
        let fp = hex::encode(&sha2::Sha256::digest(self.token.as_bytes())[..4]);
        RedactedSession {
            token_fingerprint: fp,
            user_name: self.user_name.clone(),
            client: self.client.clone(),
            device_name: self.device_name.clone(),
            app_version: self.app_version.clone(),
            created: self.created.clone(),
        }
    }
}

pub struct Sessions {
    map: RwLock<HashMap<String, Session>>,
    path: Option<PathBuf>,
}

impl Sessions {
    pub fn new(path: Option<PathBuf>) -> Self {
        let mut map = HashMap::new();
        if let Some(p) = &path {
            if let Ok(text) = std::fs::read_to_string(p) {
                if let Ok(list) = serde_json::from_str::<Vec<Session>>(&text) {
                    for s in list {
                        map.insert(s.token.clone(), s);
                    }
                }
            }
        }
        if path.is_some() {
            metrics::counter!(crate::telemetry::names::PERSIST_OPS, "store" => "sessions", "op" => "load", "outcome" => "success").increment(1);
        }
        metrics::gauge!(crate::telemetry::names::SESSIONS_ACTIVE).set(map.len() as f64);
        Sessions {
            map: RwLock::new(map),
            path,
        }
    }

    pub fn list(&self) -> Vec<RedactedSession> {
        self.map
            .read()
            .unwrap()
            .values()
            .map(|s| s.redacted())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.map.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn save(&self) {
        metrics::gauge!(crate::telemetry::names::SESSIONS_ACTIVE).set(self.len() as f64);
        if let Some(p) = &self.path {
            let list: Vec<Session> = self.map.read().unwrap().values().cloned().collect();
            if let Ok(text) = serde_json::to_string_pretty(&list) {
                let tmp = p.with_extension("tmp");
                let ok = std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, p).is_ok();
                let outcome = if ok { "success" } else { "error" };
                metrics::counter!(crate::telemetry::names::PERSIST_OPS, "store" => "sessions", "op" => "save", "outcome" => outcome).increment(1);
            }
        }
    }

    pub fn insert(&self, s: Session) {
        self.map.write().unwrap().insert(s.token.clone(), s);
        self.save();
    }

    pub fn get(&self, token: &str) -> Option<Session> {
        self.map.read().unwrap().get(token).cloned()
    }

    pub fn remove(&self, token: &str) {
        self.map.write().unwrap().remove(token);
        self.save();
    }

    /// Drop sessions whose user no longer exists in the config.
    pub fn retain_users(&self, valid_ids: &[String]) {
        self.map
            .write()
            .unwrap()
            .retain(|_, s| valid_ids.contains(&s.user_id));
        self.save();
    }
}

/// Parse `MediaBrowser Client="x", Device="y", DeviceId="z", Version="1", Token="t"`.
pub fn parse_emby_auth(value: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let v = value.trim();
    let v = match v.split_once(' ') {
        Some((scheme, rest))
            if scheme.eq_ignore_ascii_case("mediabrowser")
                || scheme.eq_ignore_ascii_case("emby") =>
        {
            rest
        }
        _ => v,
    };
    for part in v.split(',') {
        if let Some((k, val)) = part.split_once('=') {
            let k = k.trim().to_ascii_lowercase();
            let val = val.trim().trim_matches('"');
            let val = percent_decode(val);
            out.insert(k, val);
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    form_urlencoded::parse(format!("x={}", s.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| s.to_string())
}

/// Client info + token from every place Jellyfin clients put it.
pub fn client_auth(headers: &HeaderMap, query: &[(String, String)]) -> HashMap<String, String> {
    let mut info = HashMap::new();
    for h in ["x-emby-authorization", "authorization"] {
        if let Some(v) = headers.get(h).and_then(|v| v.to_str().ok()) {
            let lv = v.trim_start().to_ascii_lowercase();
            if lv.starts_with("mediabrowser") || lv.starts_with("emby") || v.contains("Token=") {
                for (k, val) in parse_emby_auth(v) {
                    info.entry(k).or_insert(val);
                }
            }
        }
    }
    if !info.contains_key("token") {
        for h in ["x-emby-token", "x-mediabrowser-token"] {
            if let Some(v) = headers.get(h).and_then(|v| v.to_str().ok()) {
                info.insert("token".into(), v.to_string());
                break;
            }
        }
    }
    if !info.contains_key("token") {
        if let Some((_, v)) = query
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("api_key") || k.eq_ignore_ascii_case("apikey"))
        {
            info.insert("token".into(), v.clone());
        }
    }
    info.retain(|_, v| !v.is_empty());
    info
}

pub fn new_token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut b);
    hex::encode(b)
}

/// Reload subscriber: drops sessions of users removed from the config.
/// Runs after the authenticator in the hub.
pub struct SessionPruner {
    pub sessions: std::sync::Arc<Sessions>,
    pub users: std::sync::Arc<dyn crate::users::ProxyAuthenticator>,
}

#[async_trait::async_trait]
impl crate::reload::Reloadable for SessionPruner {
    fn reload_name(&self) -> String {
        "sessions".into()
    }
    async fn apply(
        &self,
        cx: &crate::reload::ReloadContext<'_>,
    ) -> Result<bool, crate::error::ReloadError> {
        if cx.old.users == cx.new.users {
            return Ok(false);
        }
        let before = self.sessions.len();
        self.sessions.retain_users(&self.users.ids());
        Ok(self.sessions.len() != before)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn session_debug_redacts_token() {
        let s = super::Session {
            token: "supersecrettoken".into(),
            session_id: "s".into(),
            user_name: "u".into(),
            user_id: "i".into(),
            client: "c".into(),
            device_name: "d".into(),
            device_id: "di".into(),
            app_version: "1".into(),
            created: "t".into(),
        };
        assert!(!format!("{s:?}").contains("supersecrettoken"));
        let r = serde_json::to_string(&s.redacted()).unwrap();
        assert!(!r.contains("supersecrettoken"));
    }

    use super::*;

    #[test]
    fn token_forms() {
        let mut h = HeaderMap::new();
        h.insert(
            "authorization",
            r#"MediaBrowser Client="Android TV", Device="Shield", DeviceId="abc", Version="0.16", Token="tok1""#
                .parse()
                .unwrap(),
        );
        let i = client_auth(&h, &[]);
        assert_eq!(i["token"], "tok1");
        assert_eq!(i["client"], "Android TV");
        assert_eq!(i["deviceid"], "abc");

        let mut h = HeaderMap::new();
        h.insert("x-emby-token", "tok2".parse().unwrap());
        assert_eq!(client_auth(&h, &[])["token"], "tok2");

        let mut h = HeaderMap::new();
        h.insert("x-mediabrowser-token", "tok4".parse().unwrap());
        assert_eq!(client_auth(&h, &[])["token"], "tok4");

        let q = vec![("api_key".to_string(), "tok3".to_string())];
        assert_eq!(client_auth(&HeaderMap::new(), &q)["token"], "tok3");

        let mut h = HeaderMap::new();
        h.insert(
            "x-emby-authorization",
            r#"MediaBrowser Client="Jellyfin%20Web", Device="Firefox", DeviceId="d", Version="10.10.0""#
                .parse()
                .unwrap(),
        );
        let i = client_auth(&h, &[]);
        assert_eq!(i["client"], "Jellyfin Web");
        assert!(!i.contains_key("token"));
    }
}
