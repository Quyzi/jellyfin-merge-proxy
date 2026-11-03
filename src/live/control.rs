//! Playback state of live sessions, and what `Sessions/{id}/Playing`
//! plays (`SessionManager.UpdateNowPlayingItem`, `SendPlayCommand`).

use super::LiveSessions;
use crate::ids::Target;
use crate::index::query::Scope;
use crate::index::snapshot::Snapshot;
use crate::proxy::{self, q_get, Client, Req};
use crate::types::{normalize_hex_id, ProxyItemId};
use crate::userdata::service::PlayEvent;
use crate::{AppState, Shared};
use rand::seq::SliceRandom;
use serde_json::{json, Map, Value};
use std::sync::Arc;

const QUEUE_FETCH: usize = 100;

fn ci<'a>(body: &'a Value, key: &str) -> Option<&'a Value> {
    body.as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
        .filter(|v| !v.is_null())
}

fn ci_str(body: &Value, key: &str) -> Option<String> {
    ci(body, key).and_then(Value::as_str).map(String::from)
}

/// `PlayState` of a session from a playback report.
fn play_state(body: &Value, item: &str) -> Value {
    let mut m = Map::new();
    let flag = |k: &str| ci(body, k).and_then(Value::as_bool).unwrap_or(false);
    for k in [
        "PositionTicks",
        "VolumeLevel",
        "AudioStreamIndex",
        "SubtitleStreamIndex",
    ] {
        if let Some(v) = ci(body, k).filter(|v| v.is_number()) {
            m.insert(k.into(), v.clone());
        }
    }
    for k in ["CanSeek", "IsPaused", "IsMuted"] {
        m.insert(k.into(), json!(flag(k)));
    }
    let source = ci_str(body, "MediaSourceId").unwrap_or_else(|| item.to_string());
    m.insert("MediaSourceId".into(), json!(source));
    for k in ["PlayMethod", "LiveStreamId"] {
        if let Some(v) = ci(body, k).filter(|v| v.is_string()) {
            m.insert(k.into(), v.clone());
        }
    }
    let or = |k: &str, d: &str| ci_str(body, k).unwrap_or_else(|| d.into());
    m.insert("RepeatMode".into(), json!(or("RepeatMode", "RepeatNone")));
    m.insert(
        "PlaybackOrder".into(),
        json!(or("PlaybackOrder", "Default")),
    );
    Value::Object(m)
}

fn queue_ids(queue: &Value) -> Vec<String> {
    queue
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|e| e.get("Id").and_then(Value::as_str))
                .filter_map(normalize_hex_id)
                .take(QUEUE_FETCH)
                .collect()
        })
        .unwrap_or_default()
}

/// The (server, upstream id) an item id lives at.
fn pick(st: &Shared, id: &str) -> Option<(crate::types::ServerId, String)> {
    match st.ids.decode(&ProxyItemId::parse(id)?)? {
        Target::Item { upstream, id } => Some((upstream, id.into_string())),
        Target::View(_) => None,
    }
}

/// The DTO of an item as `c` sees it.
pub async fn item_dto(st: &Shared, c: &Client, id: &str) -> Option<Value> {
    let id = normalize_hex_id(id)?;
    let p = pick(st, &id)?;
    let mut got = proxy::fetch_dtos(st, c, &[Some(p.clone())], &[]).await;
    let mut v = got.remove(&p)?;
    v["Id"] = json!(id);
    Some(v)
}

async fn queue_dtos(st: &Shared, c: &Client, ids: &[String]) -> Vec<Value> {
    let picks: Vec<_> = ids.iter().map(|i| pick(st, i)).collect();
    let mut got = proxy::fetch_dtos(st, c, &picks, &[]).await;
    ids.iter()
        .zip(picks)
        .filter_map(|(id, p)| {
            let mut v = got.remove(&p?)?;
            v["Id"] = json!(id);
            Some(v)
        })
        .collect()
}

/// What a report needs fetched afterwards.
struct Followup {
    item: String,
    queue: Vec<String>,
}

impl LiveSessions {
    /// Apply a playback report to the session; the user to notify and the
    /// DTOs to fetch (when the item or queue changed).
    fn apply_report(
        &self,
        sid: &str,
        ev: PlayEvent,
        item: &str,
        body: &Value,
    ) -> Option<(String, Option<Followup>)> {
        let mut m = self.map.write().unwrap();
        let s = m.get_mut(sid)?;
        let mut follow = None;
        s.playlist_item_id = ci_str(body, "PlaylistItemId");
        if ev == PlayEvent::Stopped {
            s.now_playing = None;
            s.now_playing_id = None;
            s.play_state = super::default_play_state();
            if let Some(q) = ci(body, "NowPlayingQueue").filter(|q| q.is_array()) {
                s.queue = q.clone();
            }
        } else {
            s.play_state = play_state(body, item);
            let now = crate::dto::now_iso();
            s.last_check_in = Some(now.clone());
            if s.play_state["IsPaused"] == json!(true) {
                s.last_paused.get_or_insert(now);
            } else {
                s.last_paused = None;
            }
            let new_queue = ci(body, "NowPlayingQueue")
                .filter(|q| q.as_array().is_some_and(|a| !a.is_empty()) && **q != s.queue)
                .cloned();
            let queue_changed = new_queue.is_some();
            if let Some(q) = new_queue {
                s.queue = q;
            }
            if s.now_playing_id.as_deref() != Some(item) || queue_changed {
                s.now_playing_id = Some(item.to_string());
                follow = Some(Followup {
                    item: item.to_string(),
                    queue: queue_ids(&s.queue),
                });
            }
        }
        Some((s.user_id.clone(), follow))
    }

    fn set_now_playing(&self, sid: &str, item: &str, dto: Option<Value>, full: Vec<Value>) {
        let mut m = self.map.write().unwrap();
        if let Some(s) = m.get_mut(sid) {
            if s.now_playing_id.as_deref() == Some(item) {
                s.now_playing = dto;
            }
            s.queue_full = Value::Array(full);
        }
    }
}

/// Reflect a `Sessions/Playing*` report on the caller's live session.
pub fn record_session(st: &Shared, r: &Req, c: &Client, body: &Value) {
    let (Some(s), Some(ev)) = (
        c.session.as_ref(),
        r.segs.last().and_then(|s| PlayEvent::from_seg(s)),
    ) else {
        return;
    };
    let item = ci_str(body, "ItemId")
        .or_else(|| q_get(&r.query, "ItemId").map(String::from))
        .and_then(|i| normalize_hex_id(&i));
    let Some(item) = item else { return };
    let live = &st.local.live;
    let Some((user, follow)) = live.apply_report(&s.session_id, ev, &item, body) else {
        return;
    };
    if let Some(f) = follow {
        let weak = Arc::downgrade(st);
        let sid = s.session_id.clone();
        let user = user.clone();
        tokio::spawn(async move { fetch_followup(weak, sid, user, f).await });
    }
    live.notify(&user, true);
}

async fn fetch_followup(weak: std::sync::Weak<AppState>, sid: String, user: String, f: Followup) {
    let Some(st) = weak.upgrade() else { return };
    let c = Client {
        token: String::new(),
        user_id: user.clone(),
        session: None,
    };
    let (dto, full) = tokio::join!(item_dto(&st, &c, &f.item), queue_dtos(&st, &c, &f.queue));
    st.local.live.set_now_playing(&sid, &f.item, dto, full);
    st.local.live.notify(&user, true);
}

// ---------------------------------------------------------------------------
// Play command
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayCmd {
    PlayNow,
    PlayNext,
    PlayLast,
    PlayInstantMix,
    PlayShuffle,
}

impl PlayCmd {
    pub fn parse(s: &str) -> Option<Self> {
        [
            PlayCmd::PlayNow,
            PlayCmd::PlayNext,
            PlayCmd::PlayLast,
            PlayCmd::PlayInstantMix,
            PlayCmd::PlayShuffle,
        ]
        .into_iter()
        .find(|c| c.name().eq_ignore_ascii_case(s))
    }

    pub fn name(self) -> &'static str {
        match self {
            PlayCmd::PlayNow => "PlayNow",
            PlayCmd::PlayNext => "PlayNext",
            PlayCmd::PlayLast => "PlayLast",
            PlayCmd::PlayInstantMix => "PlayInstantMix",
            PlayCmd::PlayShuffle => "PlayShuffle",
        }
    }
}

/// Item ids as the target user would play them, and the command to send.
pub async fn expand_play(
    st: &Shared,
    controller: &str,
    target_user: &str,
    ids: Vec<String>,
    cmd: PlayCmd,
) -> (Vec<String>, PlayCmd) {
    let mut cmd = cmd;
    let mut items: Vec<String> = vec![];
    if cmd == PlayCmd::PlayInstantMix {
        for id in &ids {
            items.extend(instant_mix(st, target_user, id).await);
        }
        cmd = PlayCmd::PlayNow;
    } else {
        let snap = st.index.latest();
        for id in &ids {
            items.extend(expand_one(st, controller, snap.as_deref(), id));
        }
    }
    if cmd == PlayCmd::PlayShuffle {
        items.shuffle(&mut rand::thread_rng());
        cmd = PlayCmd::PlayNow;
    }
    if items.len() == 1 && st.local.settings.enable_next_episode_auto_play(target_user) {
        if let Some(rest) = st
            .index
            .latest()
            .and_then(|s| rest_of_series(&s, &items[0]))
        {
            items = rest;
        }
    }
    // Never send ids the target user may not see (as `LibraryChanged` does).
    // An id that is not a proxy id cannot be rated: it only passes for an
    // unrestricted user.
    let restricted = st.parental.restricted(target_user);
    items.retain(|i| match ProxyItemId::parse(i) {
        Some(p) => !st.parental.blocks(target_user, &p),
        None => !restricted,
    });
    (items, cmd)
}

async fn instant_mix(st: &Shared, user: &str, id: &str) -> Vec<String> {
    let Some((upstream, up_id)) = pick(st, id) else {
        return vec![];
    };
    let Some(up) = st.backend(upstream) else {
        return vec![];
    };
    let Ok(uc) = up.login_as(Some(user)).await else {
        return vec![];
    };
    let path = format!(
        "Items/{up_id}/InstantMix?UserId={}&Limit=200",
        uc.user_id.clone().unwrap_or_default()
    );
    let Ok((status, mut v)) = up.get_json_as(Some(user), &path).await else {
        return vec![];
    };
    if !status.is_success() {
        return vec![];
    }
    let tc = Client {
        token: String::new(),
        user_id: user.to_string(),
        session: None,
    };
    st.rewriters
        .rewrite_json(&mut v, &proxy::out_ctx(st, up.as_ref(), &uc, &tc));
    v.get("Items")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|i| i.get("Id").and_then(Value::as_str))
                .filter_map(normalize_hex_id)
                .collect()
        })
        .unwrap_or_default()
}

/// Episodes of a series or season (item index), in play order.
fn episodes(snap: &Snapshot, idx: usize) -> Vec<usize> {
    let mut v: Vec<usize> = snap
        .episodes_of(idx)
        .iter()
        .copied()
        .filter(|i| !snap.is_virtual(*i))
        .collect();
    v.sort_by_key(|i| {
        let e = snap.primary(&snap.items[*i]);
        (e.parent_index, e.index)
    });
    v
}

fn ids_of(snap: &Snapshot, mut idx: Vec<usize>) -> Vec<String> {
    idx.sort_by(|a, b| {
        let key = |i: &usize| snap.primary(&snap.items[*i]).sort_name.to_lowercase();
        key(a).cmp(&key(b))
    });
    idx.into_iter()
        .map(|i| snap.items[i].id.as_str().to_string())
        .collect()
}

/// `TranslateItemForPlayback`: a folder plays its items.
fn expand_one(st: &Shared, controller: &str, snap: Option<&Snapshot>, id: &str) -> Vec<String> {
    if let Some(rec) = normalize_hex_id(id).and_then(|n| st.local.playlists.get(&n)) {
        // Only a playlist the controlling user can see (a private or
        // un-shared one plays nothing).
        return if rec.visible(controller) {
            rec.items
        } else {
            vec![]
        };
    }
    let Some(snap) = snap else {
        return vec![id.to_string()];
    };
    let playable = |i: &usize| !snap.primary(&snap.items[*i]).is_folder && !snap.is_virtual(*i);
    if let Some(Scope::Libraries(libs)) = proxy::scope_of(st, snap, id) {
        let all = (0..snap.items.len())
            .filter(|i| snap.items[*i].libraries.iter().any(|l| libs.contains(l)))
            .filter(playable)
            .collect();
        return ids_of(snap, all);
    }
    let Some(pid) = ProxyItemId::parse(id) else {
        return vec![id.to_string()];
    };
    if let Some(n) = snap.name(&pid) {
        let all = (0..snap.items.len())
            .filter(|i| {
                let it = &snap.items[*i];
                it.genres.contains(&n)
                    || it.studios.contains(&n)
                    || it.people.iter().any(|p| p.0 == n)
            })
            .filter(playable)
            .collect();
        return ids_of(snap, all);
    }
    match snap.item_index(&pid) {
        Some(i)
            if snap.primary(&snap.items[i]).is("Series")
                || snap.primary(&snap.items[i]).is("Season") =>
        {
            episodes(snap, i)
                .into_iter()
                .map(|e| snap.items[e].id.as_str().to_string())
                .collect()
        }
        _ => vec![id.to_string()],
    }
}

/// The episode and every later episode of its series, when `id` is one.
fn rest_of_series(snap: &Snapshot, id: &str) -> Option<Vec<String>> {
    let i = snap.item_index(&ProxyItemId::parse(id)?)?;
    let it = &snap.items[i];
    if !snap.primary(it).is("Episode") {
        return None;
    }
    let all = episodes(snap, it.series?);
    let from = all.iter().position(|e| *e == i)?;
    Some(
        all[from..]
            .iter()
            .map(|e| snap.items[*e].id.as_str().to_string())
            .collect(),
    )
}
