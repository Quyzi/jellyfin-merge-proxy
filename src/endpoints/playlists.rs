//! Proxy-owned playlists (`/Playlists/*`, and the playlist item itself).
//! A request about an id that is not one of these playlists is declined
//! (`Flow::Next`), so upstream playlists keep being forwarded to the server
//! that owns them.

mod entries;
mod sharing;

use super::{done, Flow, HandlerResult, Route};
use crate::handler;
use crate::local::playlists::{PlaylistRec, Share};
use crate::proxy::{self, err_resp, no_content, ok_json, q_get, Client, Req};
use crate::types::normalize_hex_id;
use crate::Shared;
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};

use entries::*;
use sharing::*;

pub fn routes() -> Vec<Route> {
    vec![
        Route::auth("playlist-create", handler!(create))
            .at("playlists")
            .post(),
        Route::auth("playlist", handler!(playlist))
            .at("playlists/{id}")
            .methods(&[Method::GET, Method::HEAD, Method::POST]),
        Route::auth("playlist-users", handler!(users))
            .at("playlists/{id}/users")
            .get(),
        Route::auth("playlist-user", handler!(share))
            .at("playlists/{id}/users/*")
            .methods(&[Method::GET, Method::HEAD, Method::POST, Method::DELETE]),
        Route::auth("playlist-items", handler!(entries))
            .at("playlists/{id}/items")
            .methods(&[Method::GET, Method::HEAD, Method::POST, Method::DELETE]),
        Route::auth("playlist-move", handler!(move_entry))
            .at("playlists/{id}/items/*/move/*")
            .post(),
        Route::auth("playlist-item", handler!(item))
            .at("items/{id}")
            .at("users/{u}/items/{id}")
            .get(),
        Route::auth("playlist-delete", handler!(delete))
            .at("items/{id}")
            .methods(&[Method::DELETE]),
        Route::auth("playlist-image", handler!(no_image))
            .at("items/{id}/images/**")
            .get(),
    ]
}

const FAILED: &str = "Error processing request.";

/// An error answer: status and plain-text body.
type Fail = (StatusCode, &'static str);

const NOT_FOUND: Fail = (StatusCode::NOT_FOUND, "Playlist not found");

fn reply(e: Fail) -> HandlerResult {
    done(err_resp(e.0, e.1))
}

fn forbidden() -> Response {
    StatusCode::FORBIDDEN.into_response()
}

/// A property of a JSON body, matched case-insensitively (as ASP.NET does).
fn field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_object()?
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v)
        .filter(|v| !v.is_null())
}

fn body(r: &Req) -> Option<Value> {
    if r.body.is_empty() {
        return Some(Value::Null);
    }
    serde_json::from_slice(&r.body).ok()
}

fn csv(v: &str) -> Vec<String> {
    v.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn strings(v: Option<&Value>) -> Option<Vec<String>> {
    Some(
        v?.as_array()?
            .iter()
            .filter_map(|i| i.as_str().map(str::to_string))
            .collect(),
    )
}

/// `Users` of a create/update body; every user must be a proxy user.
fn shares(st: &Shared, v: &Value) -> Result<Vec<Share>, Fail> {
    let bad = || (StatusCode::BAD_REQUEST, FAILED);
    let Some(list) = v.as_array() else {
        return Err(bad());
    };
    list.iter()
        .map(|s| {
            let id = field(s, "UserId")
                .and_then(Value::as_str)
                .and_then(normalize_hex_id)
                .filter(|id| st.is_proxy_user(id))
                .ok_or_else(bad)?;
            let can_edit = field(s, "CanEdit")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Ok(Share {
                user_id: id,
                can_edit,
            })
        })
        .collect()
}

/// The playlist a route is about, if it is one of ours: `Ok(None)` for
/// "not ours" (decline), `Err` for ours but invisible to the caller.
fn find(st: &Shared, id: &str, c: &Client) -> Result<Option<PlaylistRec>, Fail> {
    let Some(rec) = normalize_hex_id(id).and_then(|n| st.local.playlists.get(&n)) else {
        return Ok(None);
    };
    if !rec.visible(&c.user_id) {
        return Err(NOT_FOUND);
    }
    Ok(Some(rec))
}

/// `userId` of a query: only the caller's own is accepted.
fn caller_only(r: &Req, c: &Client) -> bool {
    q_get(&r.query, "userId")
        .and_then(normalize_hex_id)
        .is_none_or(|u| u == c.user_id)
}

async fn create(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let Some(b) = body(&r) else {
        return done(err_resp(StatusCode::BAD_REQUEST, FAILED));
    };
    let user = q_get(&r.query, "userId").map(str::to_string).or_else(|| {
        field(&b, "UserId")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    if user.is_some_and(|u| normalize_hex_id(&u).as_deref() != Some(c.user_id.as_str())) {
        return done(err_resp(StatusCode::FORBIDDEN, FAILED));
    }
    let name = q_get(&r.query, "name")
        .map(str::to_string)
        .or_else(|| {
            field(&b, "Name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|n| !n.trim().is_empty());
    let Some(name) = name else {
        return done(err_resp(StatusCode::BAD_REQUEST, FAILED));
    };
    let mut ids = q_get(&r.query, "ids").map(csv).unwrap_or_default();
    if ids.is_empty() {
        ids = strings(field(&b, "Ids")).unwrap_or_default();
    }
    let media = q_get(&r.query, "mediaType")
        .map(str::to_string)
        .or_else(|| {
            field(&b, "MediaType")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    if let Some(m) = &media {
        if !["Unknown", "Video", "Audio", "Photo", "Book"]
            .iter()
            .any(|k| k.eq_ignore_ascii_case(m))
        {
            return done(err_resp(StatusCode::BAD_REQUEST, FAILED));
        }
    }
    let users = match field(&b, "Users") {
        Some(u) => match shares(st, u) {
            Ok(s) => s,
            Err(e) => return reply(e),
        },
        None => vec![],
    };
    let public = field(&b, "IsPublic")
        .and_then(Value::as_bool)
        .unwrap_or(!b.is_null());
    let Ok(items) = proxy::expand(st, c, &ids).await else {
        return done(err_resp(StatusCode::BAD_REQUEST, FAILED));
    };
    let media_type = match media.filter(|m| !m.eq_ignore_ascii_case("unknown")) {
        Some(m) => m,
        None => proxy::infer_media_type(st, c, &ids).await,
    };
    let id = crate::ids::hash_id(&[
        "playlist",
        &c.user_id,
        &hex::encode(rand::random::<[u8; 16]>()),
    ]);
    st.local.playlists.upsert(PlaylistRec {
        id: id.clone(),
        owner: c.user_id.clone(),
        name,
        media_type,
        open_access: public,
        shares: users,
        items,
        created: crate::dto::now_iso(),
    });
    done(ok_json(json!({"Id": id})))
}

async fn playlist(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let rec = match find(st, &r.segs[1], c) {
        Ok(Some(rec)) => rec,
        Ok(None) => return Ok(Flow::Next(r)),
        Err(e) => return reply(e),
    };
    if r.method == Method::POST {
        return update(st, &r, c, rec).await;
    }
    let shares: Vec<Value> = rec.shares.iter().map(share_json).collect();
    done(ok_json(
        json!({"OpenAccess": rec.open_access, "Shares": shares, "ItemIds": rec.items}),
    ))
}

fn share_json(s: &Share) -> Value {
    json!({"UserId": s.user_id, "CanEdit": s.can_edit})
}

async fn update(st: &Shared, r: &Req, c: &Client, rec: PlaylistRec) -> HandlerResult {
    if !rec.can_edit(&c.user_id) {
        return done(forbidden());
    }
    let Some(b) = body(r).filter(|b| b.is_object()) else {
        return done(err_resp(StatusCode::BAD_REQUEST, FAILED));
    };
    let users = match field(&b, "Users") {
        Some(u) => match shares(st, u) {
            Ok(s) => Some(s),
            Err(e) => return reply(e),
        },
        None => None,
    };
    let items = match strings(field(&b, "Ids")) {
        Some(ids) => match proxy::expand(st, c, &ids).await {
            Ok(items) => Some(items),
            Err(_) => return done(err_resp(StatusCode::BAD_REQUEST, FAILED)),
        },
        None => None,
    };
    let name = field(&b, "Name")
        .and_then(Value::as_str)
        .map(str::to_string);
    let public = field(&b, "IsPublic").and_then(Value::as_bool);
    st.local.playlists.update(&rec.id, |p| {
        if let Some(n) = name {
            p.name = n;
        }
        if let Some(u) = users {
            p.shares = u;
        }
        if let Some(i) = items {
            p.items = i;
        }
        if let Some(o) = public {
            p.open_access = o;
        }
    });
    done(no_content())
}

/// `GET Items/{id}` / `Users/{u}/Items/{id}` for a playlist or the
/// synthetic playlists library.
async fn item(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let id = r.segs.last().cloned().unwrap_or_default();
    let norm = normalize_hex_id(&id).unwrap_or_default();
    match find(st, &norm, c) {
        Ok(Some(rec)) => {
            let view = proxy::playlists_view_for(st, c).await;
            let mut dto = proxy::playlist_dto(st, &c.user_id, &rec, &view);
            proxy::strip_playlist_fields(&mut dto, q_get(&r.query, "Fields").unwrap_or(""));
            done(ok_json(dto))
        }
        Ok(None) if norm == proxy::playlists_view_id() => {
            done(ok_json(proxy::playlists_view_dto(st)))
        }
        Ok(None) => Ok(Flow::Next(r)),
        Err(e) => reply(e),
    }
}

async fn delete(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let rec = match find(st, &r.segs[1], c) {
        Ok(Some(rec)) => rec,
        Ok(None) => return Ok(Flow::Next(r)),
        Err(e) => return reply(e),
    };
    if rec.owner != c.user_id {
        return done(err_resp(StatusCode::UNAUTHORIZED, "Unauthorized access"));
    }
    st.local.playlists.delete(&rec.id);
    done(no_content())
}

async fn no_image(st: &Shared, r: Req, _c: &Client) -> HandlerResult {
    let id = normalize_hex_id(&r.segs[1]).unwrap_or_default();
    if st.local.playlists.is_playlist(&id) || id == proxy::playlists_view_id() {
        return done(err_resp(StatusCode::NOT_FOUND, "no image"));
    }
    Ok(Flow::Next(r))
}
