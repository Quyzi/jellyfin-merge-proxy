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
/// `ServerConfiguration.MinAudiobookResume` / `MaxAudiobookResume`.
const MIN_AUDIOBOOK_RESUME_MIN: i64 = 5;
const MAX_AUDIOBOOK_RESUME_MIN: i64 = 5;

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

/// Kinds that have no played status (`SupportsPlayedStatus` is false).
/// `None` (kind unknown) supports it.
pub fn supports_played(kind: Option<&str>) -> bool {
    !matches!(
        kind,
        Some(
            "CollectionFolder"
                | "UserView"
                | "UserRootFolder"
                | "AggregateFolder"
                | "PhotoAlbum"
                | "Photo"
                | "MusicAlbum"
                | "MusicArtist"
                | "Person"
                | "Genre"
                | "MusicGenre"
                | "Studio"
                | "Year"
                | "TvChannel"
                | "LiveTvChannel"
                | "Program"
        )
    )
}

/// Kinds that remember a resume position (`SupportsPositionTicksResume`).
pub fn resumable(kind: Option<&str>) -> bool {
    kind.is_none_or(|k| {
        matches!(
            k,
            "Movie" | "Episode" | "Video" | "MusicVideo" | "Book" | "AudioBook"
        )
    })
}

/// `POST UserPlayedItems/{id}` (`played`, with the optional `DatePlayed`) /
/// `DELETE` (`!played`): `BaseItem.MarkPlayed` / `MarkUnplayed`.
pub fn mark_played(s: &mut ItemState, played: bool, date: Option<&str>, now: &str) {
    if played {
        if date.is_some() {
            s.play_count += 1;
        }
        s.play_count = s.play_count.max(1);
        s.last_played = date
            .map(str::to_string)
            .or_else(|| s.last_played.take())
            .or_else(|| Some(now.to_string()));
        s.played = true;
    } else {
        s.play_count = 0;
        s.last_played = None;
        s.played = false;
    }
    s.position_ticks = 0;
}

/// `Sessions/Playing` (`SessionManager.OnPlaybackStart`): counts a play;
/// media with no resume position is played from the first moment.
pub fn start(s: &mut ItemState, kind: Option<&str>, now: &str) {
    s.play_count += 1;
    s.last_played = Some(now.to_string());
    s.played = supports_played(kind) && !resumable(kind);
}

/// `UserDataManager.UpdatePlayState`. Returns whether the item was played
/// to completion. Never touches `last_played` or `play_count`.
pub fn update_play_state(
    s: &mut ItemState,
    position: Option<i64>,
    runtime: Option<i64>,
    kind: Option<&str>,
    r: &ResumeRules,
) -> bool {
    let rt = runtime.unwrap_or(0);
    let mut pos = position.unwrap_or(rt);
    let mut done = false;
    let book = matches!(kind, Some("AudioBook" | "Book"));
    if pos > 0 && rt > 0 && !book {
        let pct = pos as f64 * 100.0 / rt as f64;
        if pct < r.min_pct {
            pos = 0;
        } else if pct > r.max_pct
            || pos >= rt - TICKS_PER_SEC
            || (rt / TICKS_PER_SEC) < r.min_duration_secs as i64
        {
            pos = 0;
            done = true;
        }
    } else if pos > 0 && rt > 0 && kind == Some("AudioBook") {
        let minutes_in = pos as f64 / (TICKS_PER_SEC * 60) as f64;
        let minutes_left = (rt - pos) as f64 / (TICKS_PER_SEC * 60) as f64;
        if minutes_in < MIN_AUDIOBOOK_RESUME_MIN as f64 {
            pos = 0;
        } else if minutes_left < MAX_AUDIOBOOK_RESUME_MIN as f64 || pos >= rt {
            pos = 0;
            done = true;
        }
    } else if rt <= 0 {
        pos = 0;
        done = true;
    }
    if done {
        s.played = true;
    }
    if !supports_played(kind) {
        pos = 0;
        s.played = false;
    }
    if !resumable(kind) {
        pos = 0;
    }
    s.position_ticks = pos;
    done
}

/// `Sessions/Playing/Progress`: a reported position updates the state.
pub fn progress(
    s: &mut ItemState,
    position: Option<i64>,
    runtime: Option<i64>,
    kind: Option<&str>,
    r: &ResumeRules,
) {
    if position.is_some() {
        update_play_state(s, position, runtime, kind, r);
    }
}

/// `Sessions/Playing/Stopped` (`SessionManager.OnPlaybackStopped`).
/// Returns whether the item counts as played.
pub fn stopped(
    s: &mut ItemState,
    position: Option<i64>,
    runtime: Option<i64>,
    kind: Option<&str>,
    r: &ResumeRules,
    failed: bool,
) -> bool {
    if failed {
        return s.played;
    }
    if position.is_some() {
        return update_play_state(s, position, runtime, kind, r);
    }
    s.play_count += 1;
    s.played = supports_played(kind);
    s.position_ticks = 0;
    s.played
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
