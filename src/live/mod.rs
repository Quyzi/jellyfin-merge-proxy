//! Live sessions: the proxy's own session registry (what Jellyfin's
//! `SessionManager` keeps for connected clients), their WebSockets, remote
//! control between clients, and the push notifications
//! (`UserDataChanged`, `LibraryChanged`, `Sessions`).
//!
//! Every lock here is a short-lived `std` lock, never held across `.await`.

pub mod control;
mod info;
pub mod notify;
pub mod play_sessions;
pub mod socket;

use crate::auth;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::Instant;
use tokio::sync::{broadcast, mpsc};

/// A WebSocket message: `{"MessageType", "MessageId", "Data"?}`.
pub fn envelope(kind: &str, data: Option<Value>) -> String {
    let mut m = json!({
        "MessageType": kind,
        "MessageId": hex::encode(rand::random::<[u8; 16]>()),
    });
    if let Some(d) = data {
        m["Data"] = d;
    }
    m.to_string()
}

pub fn default_play_state() -> Value {
    json!({"CanSeek": false, "IsPaused": false, "IsMuted": false,
           "RepeatMode": "RepeatNone", "PlaybackOrder": "Default"})
}

type Socket = (u64, mpsc::UnboundedSender<String>);

/// One connected client session.
pub struct LiveSession {
    session_id: String,
    token: String,
    user_id: String,
    user_name: String,
    client: String,
    device_name: String,
    device_id: String,
    app_version: String,
    last_activity: String,
    last_activity_at: Instant,
    last_check_in: Option<String>,
    last_paused: Option<String>,
    capabilities: Option<Value>,
    now_playing: Option<Value>,
    now_playing_id: Option<String>,
    play_state: Value,
    now_viewing: Option<Value>,
    queue: Value,
    queue_full: Value,
    playlist_item_id: Option<String>,
    additional_users: Vec<(String, String)>,
    sockets: Vec<Socket>,
}

impl LiveSession {
    fn new(s: &auth::Session, caps: Option<Value>) -> Self {
        LiveSession {
            session_id: s.session_id.clone(),
            token: s.token.clone(),
            user_id: s.user_id.clone(),
            user_name: s.user_name.clone(),
            client: s.client.clone(),
            device_name: s.device_name.clone(),
            device_id: s.device_id.clone(),
            app_version: s.app_version.clone(),
            last_activity: crate::dto::now_iso(),
            last_activity_at: Instant::now(),
            last_check_in: None,
            last_paused: None,
            capabilities: caps,
            now_playing: None,
            now_playing_id: None,
            play_state: default_play_state(),
            now_viewing: None,
            queue: json!([]),
            queue_full: json!([]),
            playlist_item_id: None,
            additional_users: vec![],
            sockets: vec![],
        }
    }

    fn involves(&self, user: &str) -> bool {
        self.user_id == user || self.additional_users.iter().any(|(u, _)| u == user)
    }

    /// Send to the most recently attached socket that is still open.
    fn send(&self, msg: &str) -> bool {
        self.sockets
            .iter()
            .rev()
            .any(|(_, tx)| tx.send(msg.to_string()).is_ok())
    }
}

pub struct LiveSessions {
    map: RwLock<HashMap<String, LiveSession>>,
    caps_by_device: RwLock<HashMap<String, Value>>,
    next_socket: AtomicU64,
    /// `(user id, force)`: something about the user's sessions changed.
    changed: broadcast::Sender<(String, bool)>,
}

impl Default for LiveSessions {
    fn default() -> Self {
        Self::new()
    }
}

impl LiveSessions {
    pub fn new() -> Self {
        LiveSessions {
            map: RwLock::new(HashMap::new()),
            caps_by_device: RwLock::new(HashMap::new()),
            next_socket: AtomicU64::new(1),
            changed: broadcast::channel(256).0,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<(String, bool)> {
        self.changed.subscribe()
    }

    pub fn notify(&self, user: &str, force: bool) {
        let _ = self.changed.send((user.to_string(), force));
    }

    /// Record activity of an authenticated request; creates the session
    /// (with the capabilities its device reported before) when missing.
    pub fn touch(&self, s: &auth::Session) {
        let known = self.map.read().unwrap().contains_key(&s.session_id);
        let caps = if known {
            None
        } else {
            self.caps_by_device
                .read()
                .unwrap()
                .get(&s.device_id)
                .cloned()
        };
        let mut created = false;
        {
            let mut m = self.map.write().unwrap();
            let e = m.entry(s.session_id.clone()).or_insert_with(|| {
                created = true;
                LiveSession::new(s, caps)
            });
            e.last_activity = crate::dto::now_iso();
            e.last_activity_at = Instant::now();
        }
        if created {
            self.notify(&s.user_id, true);
        }
        self.notify(&s.user_id, false);
    }

    /// Drop a session; its sockets end.
    pub fn remove(&self, session_id: &str) {
        let gone = self.map.write().unwrap().remove(session_id);
        if let Some(s) = gone {
            self.notify(&s.user_id, true);
        }
    }

    /// Drop live sessions whose token is no longer a valid session.
    pub fn retain_tokens(&self, sessions: &auth::Sessions) {
        let mut users = vec![];
        self.map.write().unwrap().retain(|_, s| {
            let keep = sessions.get(&s.token).is_some();
            if !keep {
                users.push(s.user_id.clone());
            }
            keep
        });
        for u in users {
            self.notify(&u, true);
        }
    }

    pub fn attach(&self, session_id: &str) -> Option<(u64, mpsc::UnboundedReceiver<String>)> {
        let (tx, rx) = mpsc::unbounded_channel();
        let id = self.next_socket.fetch_add(1, Ordering::Relaxed);
        let user = {
            let mut m = self.map.write().unwrap();
            let s = m.get_mut(session_id)?;
            s.sockets.push((id, tx));
            s.user_id.clone()
        };
        self.notify(&user, true);
        Some((id, rx))
    }

    /// A socket closed. A session without sockets is closed with it.
    pub fn detach(&self, session_id: &str, socket_id: u64) {
        let user = {
            let mut m = self.map.write().unwrap();
            let Some(s) = m.get_mut(session_id) else {
                return;
            };
            s.sockets.retain(|(i, _)| *i != socket_id);
            if !s.sockets.is_empty() {
                return;
            }
            let user = s.user_id.clone();
            m.remove(session_id);
            user
        };
        self.notify(&user, true);
    }

    /// Send to one session's newest open socket.
    pub fn send(&self, session_id: &str, msg: &str) -> bool {
        self.map
            .read()
            .unwrap()
            .get(session_id)
            .is_some_and(|s| s.send(msg))
    }

    /// Send to every session of (or shared with) `user`.
    pub fn send_user(&self, user: &str, msg: &str) {
        for s in self
            .map
            .read()
            .unwrap()
            .values()
            .filter(|s| s.involves(user))
        {
            s.send(msg);
        }
    }

    /// Users that have sessions.
    pub fn users(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .map
            .read()
            .unwrap()
            .values()
            .map(|s| s.user_id.clone())
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// The user a session belongs to.
    pub fn owner(&self, session_id: &str) -> Option<String> {
        self.map
            .read()
            .unwrap()
            .get(session_id)
            .map(|s| s.user_id.clone())
    }

    /// Store a device's capabilities on the session and remember them for
    /// the device's next session.
    pub fn set_caps(&self, session_id: &str, caps: Value) {
        let device = {
            let mut m = self.map.write().unwrap();
            let Some(s) = m.get_mut(session_id) else {
                return;
            };
            s.capabilities = Some(caps.clone());
            s.device_id.clone()
        };
        self.caps_by_device.write().unwrap().insert(device, caps);
    }

    pub fn set_viewing(&self, session_id: &str, item: Option<Value>) {
        if let Some(s) = self.map.write().unwrap().get_mut(session_id) {
            s.now_viewing = item;
        }
    }

    pub fn add_user(&self, session_id: &str, user: &str, name: &str) {
        if let Some(s) = self.map.write().unwrap().get_mut(session_id) {
            if !s.additional_users.iter().any(|(u, _)| u == user) {
                s.additional_users.push((user.into(), name.into()));
            }
        }
    }

    pub fn remove_user(&self, session_id: &str, user: &str) {
        if let Some(s) = self.map.write().unwrap().get_mut(session_id) {
            s.additional_users.retain(|(u, _)| u != user);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, user: &str, device: &str) -> auth::Session {
        auth::Session {
            token: format!("tok-{id}"),
            session_id: id.into(),
            user_name: user.into(),
            user_id: user.into(),
            client: "c".into(),
            device_name: "d".into(),
            device_id: device.into(),
            app_version: "1".into(),
            created: String::new(),
        }
    }

    /// A session whose first socket closed was dropped while another
    /// socket still served it, or kept after the last one closed.
    #[test]
    fn last_socket_closing_ends_the_session() {
        let live = LiveSessions::new();
        live.touch(&session("s1", "u", "dev"));
        let (a, _ra) = live.attach("s1").unwrap();
        let (b, _rb) = live.attach("s1").unwrap();
        live.detach("s1", a);
        assert_eq!(live.owner("s1").as_deref(), Some("u"));
        live.detach("s1", b);
        assert_eq!(live.owner("s1"), None);
    }

    /// Capabilities reported by a device came back after its session was
    /// recreated, as Jellyfin keeps them per device.
    #[test]
    fn capabilities_survive_session_recreation() {
        let live = LiveSessions::new();
        let s = session("s1", "u", "dev");
        live.touch(&s);
        live.set_caps("s1", json!({"SupportsMediaControl": true}));
        live.remove("s1");
        live.touch(&s);
        let m = live.map.read().unwrap();
        assert_eq!(
            m["s1"].capabilities.as_ref().unwrap()["SupportsMediaControl"],
            true
        );
    }
}
