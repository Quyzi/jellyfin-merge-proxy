//! `SessionInfoDto` and the `GET Sessions` visibility rules.

use super::*;
use crate::types::normalize_hex_id;
use axum::http::StatusCode;

const NEVER: &str = "0001-01-01T00:00:00.0000000Z";

fn default_caps() -> Value {
    json!({"PlayableMediaTypes": [], "SupportedCommands": [],
           "SupportsMediaControl": false, "SupportsPersistentIdentifier": true})
}

impl LiveSession {
    /// `SessionInfo.SupportsRemoteControl`: the device said it can be
    /// controlled and a socket is there to carry the commands.
    fn supports_remote(&self) -> bool {
        let media = self
            .capabilities
            .as_ref()
            .and_then(|c| c.get("SupportsMediaControl"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        media && !self.sockets.is_empty()
    }

    fn dto(&self, server_id: &str) -> Value {
        let caps = self.capabilities.clone().unwrap_or_else(default_caps);
        let list = |k: &str| caps.get(k).cloned().unwrap_or_else(|| json!([]));
        let remote = self.supports_remote();
        let mut v = json!({
            "PlayState": self.play_state,
            "AdditionalUsers": self.additional_users.iter()
                .map(|(id, name)| json!({"UserId": id, "UserName": name}))
                .collect::<Vec<_>>(),
            "Capabilities": caps,
            "RemoteEndPoint": "",
            "PlayableMediaTypes": list("PlayableMediaTypes"),
            "Id": self.session_id,
            "UserId": self.user_id,
            "UserName": self.user_name,
            "Client": self.client,
            "LastActivityDate": self.last_activity,
            "LastPlaybackCheckIn": self.last_check_in.as_deref().unwrap_or(NEVER),
            "DeviceName": self.device_name,
            "DeviceId": self.device_id,
            "ApplicationVersion": self.app_version,
            "IsActive": true,
            "SupportsMediaControl": remote,
            "SupportsRemoteControl": remote,
            "NowPlayingQueue": self.queue,
            "NowPlayingQueueFullItems": self.queue_full,
            "HasCustomDeviceName": false,
            "ServerId": server_id,
            "SupportedCommands": list("SupportedCommands"),
        });
        let optional = [
            (
                "LastPausedDate",
                self.last_paused.as_ref().map(|s| json!(s)),
            ),
            ("NowPlayingItem", self.now_playing.clone()),
            ("NowViewingItem", self.now_viewing.clone()),
            (
                "PlaylistItemId",
                self.playlist_item_id.as_ref().map(|s| json!(s)),
            ),
        ];
        for (k, o) in optional {
            if let Some(o) = o {
                v[k] = o;
            }
        }
        v
    }
}

impl LiveSessions {
    pub fn dto_of(&self, session_id: &str, server_id: &str) -> Option<Value> {
        self.map
            .read()
            .unwrap()
            .get(session_id)
            .map(|s| s.dto(server_id))
    }

    /// The sessions `caller` may list (`SessionController.GetSessions`):
    /// `controllableByUserId`, `deviceId`, `activeWithinSeconds`; newest
    /// activity first. JMP users are never administrators, so they only
    /// see their own sessions (and ones they are an additional user of).
    pub fn visible(
        &self,
        caller: &str,
        q: &[(String, String)],
        server_id: &str,
    ) -> Result<Vec<Value>, StatusCode> {
        let get = |k: &str| crate::proxy::q_get(q, k);
        let controllable = get("controllableByUserId");
        if let Some(raw) = controllable {
            if normalize_hex_id(raw).as_deref() != Some(caller) {
                return Err(StatusCode::FORBIDDEN);
            }
        }
        let device = get("deviceId");
        let within = get("activeWithinSeconds")
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0);
        let map = self.map.read().unwrap();
        let mut list: Vec<&LiveSession> = map
            .values()
            .filter(|s| !(controllable.is_some() && !s.supports_remote()))
            .filter(|s| device.is_none_or(|d| s.device_id.eq_ignore_ascii_case(d)))
            .filter(|s| s.involves(caller))
            .filter(|s| within.is_none_or(|n| s.last_activity_at.elapsed().as_secs() <= n))
            .collect();
        list.sort_by(|a, b| b.last_activity.cmp(&a.last_activity));
        Ok(list.into_iter().map(|s| s.dto(server_id)).collect())
    }
}
