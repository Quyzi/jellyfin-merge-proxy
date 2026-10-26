//! Sessions, playback reporting and display preferences.

use super::{done, HandlerResult, Route};
use crate::dto;
use crate::handler;
use crate::proxy::{self, no_content, ok_json, Client, Req};
use crate::Shared;
use serde_json::{json, Value};

pub fn routes() -> Vec<Route> {
    vec![
        Route::auth("capabilities", handler!(accept))
            .at("sessions/capabilities")
            .at("sessions/capabilities/full"),
        Route::auth("sessions", handler!(list)).at("sessions").get(),
        Route::auth("logout", handler!(logout)).at("sessions/logout"),
        Route::auth("playing-ping", handler!(accept)).at("sessions/playing/ping"),
        Route::auth("playstate", handler!(playstate))
            .at("sessions/playing")
            .at("sessions/playing/progress")
            .at("sessions/playing/stopped"),
        Route::auth("display-preferences", handler!(display_prefs)).at("displaypreferences/*"),
    ]
}

async fn accept(_st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done(no_content())
}

async fn list(st: &Shared, _r: Req, c: &Client) -> HandlerResult {
    let v: Vec<Value> = c
        .session
        .iter()
        .map(|s| dto::session_info(s, &st.server_id))
        .collect();
    done(ok_json(json!(v)))
}

async fn logout(st: &Shared, _r: Req, c: &Client) -> HandlerResult {
    st.sessions.remove(&c.token);
    done(no_content())
}

async fn playstate(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    done(proxy::playstate(st, r, c).await?)
}

async fn display_prefs(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    done(proxy::display_prefs(st, &r, c))
}
