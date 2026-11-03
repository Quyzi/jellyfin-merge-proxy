//! Which upstream server owns a `PlaySessionId`.
//!
//! `PlaySessionId`s are minted by an upstream's `PlaybackInfo`, and the
//! proxy does not rewrite them (`rewrite::SKIP_ID_KEYS`), so a later
//! `DELETE Videos/ActiveEncodings` or `Sessions/Playing/Ping` carries no
//! item id to route by. [`PlaySessionRecorder`] notes the owner when the
//! `PlaybackInfo` response passes through; [`PlaySessions::owner`] looks it up.

use crate::rewrite::{OutCtx, Payload, PayloadBody, ResponseRewriter};
use crate::types::ServerId;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a play session stays known (a transcode that outlives this is
/// long past any client ping).
pub const TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Default)]
pub struct PlaySessions {
    map: Mutex<HashMap<String, (ServerId, Instant)>>,
}

impl PlaySessions {
    /// Remember that `server` issued `play_session_id`; drops expired entries.
    pub fn record(&self, play_session_id: &str, server: ServerId) {
        self.record_at(play_session_id, server, Instant::now());
    }

    /// The server that issued `play_session_id`, if still known.
    pub fn owner(&self, play_session_id: &str) -> Option<ServerId> {
        self.owner_at(play_session_id, Instant::now())
    }

    pub(crate) fn record_at(&self, play_session_id: &str, server: ServerId, now: Instant) {
        let mut m = self.map.lock().unwrap();
        m.retain(|_, (_, at)| now.saturating_duration_since(*at) < TTL);
        m.insert(play_session_id.to_string(), (server, now));
    }

    pub(crate) fn owner_at(&self, play_session_id: &str, now: Instant) -> Option<ServerId> {
        let m = self.map.lock().unwrap();
        m.get(play_session_id)
            .filter(|(_, at)| now.saturating_duration_since(*at) < TTL)
            .map(|(s, _)| *s)
    }
}

/// Records the owner of every `PlaybackInfoResponse` that passes through.
pub struct PlaySessionRecorder(pub Arc<PlaySessions>);

fn play_session_of(v: &Value) -> Option<&str> {
    let o = v.as_object()?;
    o.get("MediaSources")?.as_array()?;
    o.get("PlaySessionId")?.as_str().filter(|s| !s.is_empty())
}

impl ResponseRewriter for PlaySessionRecorder {
    fn name(&self) -> &'static str {
        "play_sessions"
    }
    fn applies(&self, p: &Payload<'_>) -> bool {
        matches!(&p.body, Some(PayloadBody::Json(v)) if play_session_of(v).is_some())
    }
    fn rewrite(&self, p: &mut Payload<'_>, cx: &OutCtx<'_>) {
        if let Some(PayloadBody::Json(v)) = &p.body {
            if let Some(id) = play_session_of(v) {
                self.0.record(id, cx.upstream);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// An expired play session must not keep routing a ping to a server
    /// whose transcode is long gone.
    #[test]
    fn entries_expire_after_the_ttl() {
        let ps = PlaySessions::default();
        let t0 = Instant::now();
        ps.record_at("abc", ServerId(1), t0);
        assert_eq!(
            ps.owner_at("abc", t0 + TTL - Duration::from_secs(1)),
            Some(ServerId(1))
        );
        assert_eq!(ps.owner_at("abc", t0 + TTL), None);
        ps.record_at("def", ServerId(2), t0 + TTL);
        assert_eq!(ps.owner_at("abc", t0 + TTL), None);
        assert_eq!(
            ps.map.lock().unwrap().len(),
            1,
            "expired entry pruned on insert"
        );
    }

    /// Only a PlaybackInfoResponse (PlaySessionId + MediaSources) names an
    /// owner; any other JSON that merely mentions a PlaySessionId must not.
    #[test]
    fn only_playback_info_responses_are_recorded() {
        let ok = json!({"MediaSources": [], "PlaySessionId": "p1"});
        assert_eq!(play_session_of(&ok), Some("p1"));
        assert_eq!(play_session_of(&json!({"PlaySessionId": "p1"})), None);
        assert_eq!(
            play_session_of(&json!({"MediaSources": [], "PlaySessionId": ""})),
            None
        );
        assert_eq!(play_session_of(&json!([ok])), None);
    }
}
