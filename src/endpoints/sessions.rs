//! Sessions, capabilities, playback reporting and display preferences.

use super::{done, HandlerResult, Route};
use crate::handler;
use crate::live::control;
use crate::proxy::{self, err_resp, no_content, ok_json, q_get, Client, Req};
use crate::types::normalize_hex_id;
use crate::Shared;
use axum::http::{Method, StatusCode};
use axum::response::Response;
use serde_json::{json, Map, Value};

pub fn routes() -> Vec<Route> {
    vec![
        Route::auth("capabilities", handler!(capabilities))
            .at("sessions/capabilities")
            .at("sessions/capabilities/full")
            .post(),
        Route::auth("viewing", handler!(viewing))
            .at("sessions/viewing")
            .post(),
        Route::auth("sessions", handler!(list)).at("sessions").get(),
        Route::auth("logout", handler!(logout)).at("sessions/logout"),
        Route::auth("playing-ping", handler!(playing_ping))
            .at("sessions/playing/ping")
            .post(),
        Route::auth("playstate", handler!(playstate))
            .at("sessions/playing")
            .at("sessions/playing/progress")
            .at("sessions/playing/stopped"),
        Route::auth("display-preferences", handler!(display_prefs))
            .at("displaypreferences/*")
            .methods(&[Method::GET, Method::POST]),
    ]
}

async fn playing_ping(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    done(proxy::playing_ping(st, r, c).await?)
}

fn not_found() -> Response {
    err_resp(StatusCode::NOT_FOUND, "Error processing request.")
}

/// The session a request acts on: the caller's own, or `?{key}=` when it is
/// one of the caller's (`SessionManager.ReportCapabilities` throws
/// otherwise).
fn target_session(st: &Shared, r: &Req, c: &Client, key: &str) -> Option<String> {
    match q_get(&r.query, key) {
        None => c.session.as_ref().map(|s| s.session_id.clone()),
        Some(id) => normalize_hex_id(id)
            .filter(|id| st.local.live.owner(id).as_deref() == Some(c.user_id.as_str())),
    }
}

fn csv(r: &Req, key: &str) -> Vec<Value> {
    q_get(&r.query, key)
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| json!(s))
                .collect()
        })
        .unwrap_or_default()
}

/// `ClientCapabilities` from the query string.
fn query_caps(r: &Req) -> Value {
    let flag = |k: &str, d: bool| {
        q_get(&r.query, k)
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(d)
    };
    json!({
        "PlayableMediaTypes": csv(r, "playableMediaTypes"),
        "SupportedCommands": csv(r, "supportedCommands"),
        "SupportsMediaControl": flag("supportsMediaControl", false),
        "SupportsPersistentIdentifier": flag("supportsPersistentIdentifier", true),
    })
}

/// `ClientCapabilitiesDto` from a request body.
fn body_caps(body: &[u8]) -> Option<Value> {
    const KEYS: &[&str] = &[
        "PlayableMediaTypes",
        "SupportedCommands",
        "SupportsMediaControl",
        "SupportsPersistentIdentifier",
        "DeviceProfile",
        "AppStoreUrl",
        "IconUrl",
    ];
    let Value::Object(b) = serde_json::from_slice::<Value>(body).ok()? else {
        return None;
    };
    let mut m = Map::new();
    m.insert("PlayableMediaTypes".into(), json!([]));
    m.insert("SupportedCommands".into(), json!([]));
    m.insert("SupportsMediaControl".into(), json!(false));
    m.insert("SupportsPersistentIdentifier".into(), json!(true));
    for (k, v) in b {
        if let Some(name) = KEYS.iter().find(|n| n.eq_ignore_ascii_case(&k)) {
            if !v.is_null() {
                m.insert((*name).into(), v);
            }
        }
    }
    Some(Value::Object(m))
}

async fn capabilities(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let Some(sid) = target_session(st, &r, c, "id") else {
        return done(not_found());
    };
    let caps = if r
        .segs
        .last()
        .is_some_and(|s| s.eq_ignore_ascii_case("full"))
    {
        match body_caps(&r.body) {
            Some(v) => v,
            None => {
                return done(err_resp(
                    StatusCode::BAD_REQUEST,
                    "Error processing request.",
                ))
            }
        }
    } else {
        query_caps(&r)
    };
    st.local.live.set_caps(&sid, caps);
    st.local.live.notify(&c.user_id, true);
    done(no_content())
}

async fn viewing(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let Some(item) = q_get(&r.query, "itemId") else {
        return done(err_resp(
            StatusCode::BAD_REQUEST,
            "Error processing request.",
        ));
    };
    let Some(sid) = target_session(st, &r, c, "sessionId") else {
        return done(not_found());
    };
    let dto = control::item_dto(st, c, item).await;
    st.local.live.set_viewing(&sid, dto);
    st.local.live.notify(&c.user_id, true);
    done(no_content())
}

async fn list(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    match st.local.live.visible(&c.user_id, &r.query, &st.server_id) {
        Ok(v) => done(ok_json(json!(v))),
        Err(code) => done(err_resp(code, "Error processing request.")),
    }
}

async fn logout(st: &Shared, _r: Req, c: &Client) -> HandlerResult {
    st.sessions.remove(&c.token);
    if let Some(s) = &c.session {
        st.local.live.remove(&s.session_id);
    }
    done(no_content())
}

async fn playstate(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    done(proxy::playstate(st, r, c).await?)
}

async fn display_prefs(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    done(crate::local::prefs::handle(st, r, c).await)
}
