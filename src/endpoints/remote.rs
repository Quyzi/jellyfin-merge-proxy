//! Remote control between a user's clients (`Sessions/{id}/...`): commands
//! are pushed to the target session's WebSocket.
//!
//! Jellyfin lets anyone with a session id control it; session ids are only
//! discoverable by the same user, so JMP restricts control to the caller's
//! own sessions (other users' sessions answer 404).

use super::{done, HandlerResult, Route};
use crate::handler;
use crate::live::control::{expand_play, PlayCmd};
use crate::live::envelope;
use crate::proxy::{err_resp, no_content, q_get, Client, Req};
use crate::types::normalize_hex_id;
use crate::Shared;
use axum::http::{Method, StatusCode};
use axum::response::Response;
use serde_json::{json, Map, Value};

const NOBODY: &str = "00000000000000000000000000000000";
const GENERIC: &str = "Error processing request.";

/// `GeneralCommandType` (`GeneralCommandType.cs`).
const GENERAL_COMMANDS: &[&str] = &[
    "MoveUp",
    "MoveDown",
    "MoveLeft",
    "MoveRight",
    "PageUp",
    "PageDown",
    "PreviousLetter",
    "NextLetter",
    "ToggleOsd",
    "ToggleContextMenu",
    "Select",
    "Back",
    "TakeScreenshot",
    "SendKey",
    "SendString",
    "GoHome",
    "GoToSettings",
    "VolumeUp",
    "VolumeDown",
    "Mute",
    "Unmute",
    "ToggleMute",
    "SetVolume",
    "SetAudioStreamIndex",
    "SetSubtitleStreamIndex",
    "ToggleFullscreen",
    "DisplayContent",
    "GoToSearch",
    "DisplayMessage",
    "SetRepeatMode",
    "ChannelUp",
    "ChannelDown",
    "Guide",
    "ToggleStats",
    "PlayMediaSource",
    "PlayTrailers",
    "SetShuffleQueue",
    "PlayState",
    "PlayNext",
    "ToggleOsdMenu",
    "Play",
    "SetMaxStreamingBitrate",
    "SetPlaybackOrder",
];

/// `PlaystateCommand`.
const PLAYSTATE_COMMANDS: &[&str] = &[
    "Stop",
    "Pause",
    "Unpause",
    "NextTrack",
    "PreviousTrack",
    "Seek",
    "Rewind",
    "FastForward",
    "PlayPause",
];

/// `BaseItemKind`.
const ITEM_KINDS: &[&str] = &[
    "AggregateFolder",
    "Audio",
    "AudioBook",
    "BasePluginFolder",
    "Book",
    "BoxSet",
    "Channel",
    "ChannelFolderItem",
    "CollectionFolder",
    "Episode",
    "Folder",
    "Genre",
    "ManualPlaylistsFolder",
    "Movie",
    "LiveTvChannel",
    "LiveTvProgram",
    "MusicAlbum",
    "MusicArtist",
    "MusicGenre",
    "MusicVideo",
    "Person",
    "Photo",
    "PhotoAlbum",
    "Playlist",
    "PlaylistsFolder",
    "Program",
    "Recording",
    "Season",
    "Series",
    "Studio",
    "Trailer",
    "TvChannel",
    "TvProgram",
    "UserRootFolder",
    "UserView",
    "Video",
    "Year",
];

pub fn routes() -> Vec<Route> {
    vec![
        Route::auth("session-play", handler!(play))
            .at("sessions/{id}/playing")
            .post(),
        Route::auth("session-playstate", handler!(playstate))
            .at("sessions/{id}/playing/*")
            .post(),
        Route::auth("session-command", handler!(named_command))
            .at("sessions/{id}/system/*")
            .at("sessions/{id}/command/*")
            .post(),
        Route::auth("session-full-command", handler!(full_command))
            .at("sessions/{id}/command")
            .post(),
        Route::auth("session-message", handler!(message))
            .at("sessions/{id}/message")
            .post(),
        Route::auth("session-viewing", handler!(browse))
            .at("sessions/{id}/viewing")
            .post(),
        Route::auth("session-user", handler!(additional_user))
            .at("sessions/{id}/user/*")
            .methods(&[Method::POST, Method::DELETE]),
    ]
}

fn bad() -> Response {
    err_resp(StatusCode::BAD_REQUEST, GENERIC)
}

/// The caller's session `segs[1]`, or the 404 Jellyfin gives for an unknown one.
fn target(st: &Shared, r: &Req, c: &Client) -> Option<String> {
    normalize_hex_id(&r.segs[1])
        .filter(|id| st.local.live.owner(id).as_deref() == Some(c.user_id.as_str()))
}

fn not_found() -> Response {
    err_resp(StatusCode::NOT_FOUND, GENERIC)
}

fn canonical(list: &[&'static str], s: &str) -> Option<&'static str> {
    list.iter().find(|n| n.eq_ignore_ascii_case(s)).copied()
}

/// Push `kind` with `data` to the target session (a session without a
/// socket is not an error: Jellyfin sends to nobody).
fn push(st: &Shared, sid: &str, kind: &str, data: Value) -> Response {
    st.local.live.send(sid, &envelope(kind, Some(data)));
    no_content()
}

fn general(name: &str, controlling: &str, arguments: Value) -> Value {
    json!({"Name": name, "ControllingUserId": controlling, "Arguments": arguments})
}

fn opt<T: serde::Serialize>(m: &mut Map<String, Value>, k: &str, v: Option<T>) {
    if let Some(v) = v {
        m.insert(k.into(), json!(v));
    }
}

async fn play(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let q = |k: &str| q_get(&r.query, k);
    let Some(cmd) = q("playCommand").and_then(PlayCmd::parse) else {
        return done(bad());
    };
    let ids: Vec<String> = q("itemIds")
        .map(|v| {
            v.split(',')
                .filter_map(|s| normalize_hex_id(s.trim()))
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        return done(bad());
    }
    let num = |k: &str| q(k).map(|v| v.parse::<i64>());
    let (Some(start), Some(audio), Some(sub), Some(index)) = (
        transpose(num("startPositionTicks")),
        transpose(num("audioStreamIndex")),
        transpose(num("subtitleStreamIndex")),
        transpose(num("startIndex")),
    ) else {
        return done(bad());
    };
    let Some(sid) = target(st, &r, c) else {
        return done(not_found());
    };
    let Some(target_user) = st.local.live.owner(&sid) else {
        return done(not_found());
    };
    let (items, cmd) = expand_play(st, &c.user_id, &target_user, ids, cmd).await;
    let mut m = Map::new();
    m.insert("ItemIds".into(), json!(items));
    opt(&mut m, "StartPositionTicks", start);
    m.insert("PlayCommand".into(), json!(cmd.name()));
    m.insert("ControllingUserId".into(), json!(c.user_id));
    opt(&mut m, "SubtitleStreamIndex", sub);
    opt(&mut m, "AudioStreamIndex", audio);
    opt(&mut m, "MediaSourceId", q("mediaSourceId"));
    opt(&mut m, "StartIndex", index);
    done(push(st, &sid, "Play", Value::Object(m)))
}

/// `Some(None)` for an absent value, `Some(Some(v))` for a parsed one and
/// `None` for one that does not parse.
fn transpose<T, E>(v: Option<Result<T, E>>) -> Option<Option<T>> {
    match v {
        None => Some(None),
        Some(Ok(v)) => Some(Some(v)),
        Some(Err(_)) => None,
    }
}

async fn playstate(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let Some(cmd) = canonical(PLAYSTATE_COMMANDS, &r.segs[3]) else {
        return done(bad());
    };
    let Some(seek) = transpose(q_get(&r.query, "seekPositionTicks").map(|v| v.parse::<i64>()))
    else {
        return done(bad());
    };
    let Some(sid) = target(st, &r, c) else {
        return done(not_found());
    };
    let mut m = Map::new();
    m.insert("Command".into(), json!(cmd));
    opt(&mut m, "SeekPositionTicks", seek);
    m.insert("ControllingUserId".into(), json!(c.user_id));
    done(push(st, &sid, "Playstate", Value::Object(m)))
}

async fn named_command(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let Some(name) = canonical(GENERAL_COMMANDS, &r.segs[3]) else {
        return done(bad());
    };
    let Some(sid) = target(st, &r, c) else {
        return done(not_found());
    };
    done(push(
        st,
        &sid,
        "GeneralCommand",
        general(name, &c.user_id, json!({})),
    ))
}

async fn full_command(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
    let name = body
        .get("Name")
        .and_then(Value::as_str)
        .and_then(|n| canonical(GENERAL_COMMANDS, n));
    let Some(name) = name else {
        return done(bad());
    };
    let args = body
        .get("Arguments")
        .filter(|a| a.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let Some(sid) = target(st, &r, c) else {
        return done(not_found());
    };
    done(push(
        st,
        &sid,
        "GeneralCommand",
        general(name, &c.user_id, args),
    ))
}

async fn message(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
    let text = body.get("Text").and_then(Value::as_str).unwrap_or("");
    if text.is_empty() {
        return done(bad());
    }
    let header = body
        .get("Header")
        .and_then(Value::as_str)
        .filter(|h| !h.trim().is_empty())
        .unwrap_or("Message from Server");
    let mut args = json!({"Header": header, "Text": text});
    if let Some(ms) = body.get("TimeoutMs").and_then(Value::as_i64) {
        args["TimeoutMs"] = json!(ms.to_string());
    }
    let Some(sid) = target(st, &r, c) else {
        return done(not_found());
    };
    let data = general("DisplayMessage", NOBODY, args);
    done(push(st, &sid, "GeneralCommand", data))
}

async fn browse(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let q = |k: &str| q_get(&r.query, k);
    let kind = q("itemType").and_then(|k| canonical(ITEM_KINDS, k));
    let (Some(kind), Some(item), Some(name)) = (kind, q("itemId"), q("itemName")) else {
        return done(bad());
    };
    let Some(sid) = target(st, &r, c) else {
        return done(not_found());
    };
    let args = json!({"ItemId": item, "ItemName": name, "ItemType": kind});
    done(push(
        st,
        &sid,
        "GeneralCommand",
        general("DisplayContent", NOBODY, args),
    ))
}

async fn additional_user(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let Some(sid) = target(st, &r, c) else {
        return done(not_found());
    };
    let user = normalize_hex_id(&r.segs[3]).and_then(|id| st.authenticator.by_id(&id));
    let Some(user) = user else {
        return done(not_found());
    };
    if user.id == c.user_id {
        return done(bad());
    }
    let live = &st.local.live;
    if r.method == Method::POST {
        live.add_user(&sid, &user.id, &user.name);
    } else {
        live.remove_user(&sid, &user.id);
    }
    live.notify(&c.user_id, true);
    done(no_content())
}
