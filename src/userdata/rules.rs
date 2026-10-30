//! Jellyfin's play-state rules, applied to proxy-held [`ItemState`].
//!
//! Mirrors Jellyfin's `UserDataManager`/`SessionManager`: starting
//! playback counts a play and clears `Played` (for resumable media);
//! progress records the position; stopping applies the resume thresholds
//! (`[userdata] min_resume_pct` / `max_resume_pct` /
//! `min_resume_duration_secs`) and marks the item played when it was
//! finished.

use super::ItemState;
use crate::config::UserDataConfig;
use serde_json::Value;

const TICKS_PER_SEC: i64 = 10_000_000;

/// Resume thresholds.
#[derive(Debug, Clone, Copy)]
pub struct ResumeRules {
    pub min_pct: f64,
    pub max_pct: f64,
    pub min_duration_secs: u64,
}

impl From<&UserDataConfig> for ResumeRules {
    fn from(c: &UserDataConfig) -> Self {
        ResumeRules {
            min_pct: c.min_resume_pct,
            max_pct: c.max_resume_pct,
            min_duration_secs: c.min_resume_duration_secs,
        }
    }
}

impl Default for ResumeRules {
    fn default() -> Self {
        (&UserDataConfig::default()).into()
    }
}

/// `POST UserPlayedItems/{id}` (`played`) / `DELETE` (`!played`).
pub fn mark_played(s: &mut ItemState, played: bool, now: &str) {
    if played {
        s.play_count += 1;
        s.last_played = Some(now.to_string());
        s.played = true;
    } else {
        s.played = false;
        s.play_count = 0;
    }
    s.position_ticks = 0;
}

/// `Sessions/Playing`.
pub fn start(s: &mut ItemState, now: &str) {
    s.play_count += 1;
    s.last_played = Some(now.to_string());
    s.played = false;
}

/// `Sessions/Playing/Progress`.
pub fn progress(s: &mut ItemState, position: Option<i64>) {
    if let Some(p) = position {
        s.position_ticks = p.max(0);
    }
}

/// `Sessions/Playing/Stopped`. Returns whether the item counts as played
/// to completion.
pub fn stopped(
    s: &mut ItemState,
    position: Option<i64>,
    runtime: Option<i64>,
    rules: &ResumeRules,
    now: &str,
) -> bool {
    let runtime = runtime.filter(|r| *r > 0);
    let mut pos = position.or(runtime).unwrap_or(0).max(0);
    let mut done = false;
    match runtime {
        Some(rt) if pos > 0 => {
            let pct = pos as f64 * 100.0 / rt as f64;
            if pct < rules.min_pct {
                pos = 0;
            } else if pct > rules.max_pct
                || pos >= rt
                || rt / TICKS_PER_SEC < rules.min_duration_secs as i64
            {
                // Watched to the end, or too short to resume.
                pos = 0;
                done = true;
            }
        }
        Some(_) => {}
        None => {
            pos = 0;
            done = true;
        }
    }
    s.position_ticks = pos;
    if done {
        s.played = true;
        s.last_played = Some(now.to_string());
    }
    done
}

/// `POST UserItems/{id}/UserData` (an `UpdateUserItemDataDto`): set the
/// fields present in the body.
pub fn update_from(s: &mut ItemState, body: &Value) {
    let get = |k: &str| {
        body.as_object().and_then(|o| {
            o.iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(k))
                .map(|(_, v)| v)
        })
    };
    if let Some(v) = get("Played").and_then(Value::as_bool) {
        s.played = v;
    }
    if let Some(v) = get("PlayCount").and_then(Value::as_i64) {
        s.play_count = v.max(0);
    }
    if let Some(v) = get("PlaybackPositionTicks").and_then(Value::as_i64) {
        s.position_ticks = v.max(0);
    }
    if let Some(v) = get("IsFavorite").and_then(Value::as_bool) {
        s.favorite = v;
    }
    match get("Likes") {
        Some(Value::Bool(b)) => s.likes = Some(*b),
        Some(Value::Null) => s.likes = None,
        _ => {}
    }
    match get("Rating") {
        Some(v) if v.is_number() => s.rating = v.as_f64(),
        Some(Value::Null) => s.rating = None,
        _ => {}
    }
    if let Some(v) = get("LastPlayedDate").and_then(Value::as_str) {
        s.last_played = Some(v.to_string());
    }
}

/// State as an upstream reported it in a DTO's `UserData` (for seeding).
pub fn from_upstream(ud: &Value) -> ItemState {
    let mut s = ItemState::default();
    update_from(&mut s, ud);
    s
}
