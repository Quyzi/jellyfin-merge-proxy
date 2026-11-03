//! Per-user item data: played, favourite, like/dislike and the generic
//! `UserData` endpoint. With `[userdata] enabled` the proxy owns this
//! state (per proxy user, keyed by canonical id), answers from it and
//! writes the change through to every live copy; otherwise these
//! requests decline to the fallback (forwarded to the owning server).

use super::{done, Flow, HandlerResult, Route};
use crate::handler;
use crate::proxy::{self, ok_json, q_get, Client, Req};
use crate::types::ProxyItemId;
use crate::userdata::{rules, view};
use crate::Shared;
use axum::http::{Method, StatusCode};
use axum::response::IntoResponse;
use serde_json::Value;

const RW: &[Method] = &[Method::POST, Method::DELETE];

pub fn routes() -> Vec<Route> {
    vec![
        Route::auth("played-items", handler!(played))
            .at("userplayeditems/{id}")
            .at("users/{u}/playeditems/{id}")
            .methods(RW),
        Route::auth("favorite-items", handler!(favorite))
            .at("userfavoriteitems/{id}")
            .at("users/{u}/favoriteitems/{id}")
            .methods(RW),
        Route::auth("item-rating", handler!(rating))
            .at("useritems/{id}/rating")
            .at("users/{u}/items/{id}/rating")
            .methods(RW),
        Route::auth("item-userdata", handler!(userdata))
            .at("useritems/{id}/userdata")
            .at("users/{u}/items/{id}/userdata")
            .methods(&[Method::GET, Method::HEAD, Method::POST]),
    ]
}

/// The item id of the path, when the proxy owns user data.
fn target(st: &Shared, r: &Req, c: &Client) -> Option<ProxyItemId> {
    if !st.userdata.enabled() || c.user_id.is_empty() {
        return None;
    }
    r.segs.iter().find_map(|s| {
        let n = crate::types::normalize_hex_id(s)?;
        if st.is_proxy_user(&n) {
            return None;
        }
        ProxyItemId::parse(&n)
    })
}

async fn reply(st: &Shared, r: &Req, c: &Client, id: &ProxyItemId) -> HandlerResult {
    proxy::write_through(st, r, c, id).await;
    let v = view::user_dto(&st.userdata, &c.user_id, id, None, None);
    done(ok_json(v))
}

/// `DatePlayed` as Jellyfin's `LegacyDateTimeModelBinder` + `DateTime.Parse`
/// read it, normalised to the DTO timestamp format (UTC).
fn parse_date_played(v: &str) -> Option<String> {
    use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
    let utc = |n: NaiveDateTime| n.and_utc();
    let t: DateTime<Utc> = NaiveDateTime::parse_from_str(v, "%Y%m%d%H%M%S")
        .map(utc)
        .ok()
        .or_else(|| {
            DateTime::parse_from_rfc3339(v)
                .ok()
                .map(|d| d.with_timezone(&Utc))
        })
        .or_else(|| {
            ["%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"]
                .iter()
                .find_map(|f| NaiveDateTime::parse_from_str(v, f).ok())
                .map(utc)
        })
        .or_else(|| {
            NaiveDate::parse_from_str(v, "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(utc)
        })?;
    Some(t.format("%Y-%m-%dT%H:%M:%S%.6f0Z").to_string())
}

async fn played(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let Some(id) = target(st, &r, c) else {
        return Ok(Flow::Next(r));
    };
    let on = r.method == Method::POST;
    let date = match q_get(&r.query, "DatePlayed") {
        Some(v) => match parse_date_played(v) {
            Some(d) => Some(d),
            None => {
                return done(proxy::err_resp(
                    StatusCode::BAD_REQUEST,
                    "Error processing request.",
                ))
            }
        },
        None => None,
    };
    let now = crate::dto::now_iso();
    st.userdata
        .set_played(&c.user_id, &id, on, date.as_deref().filter(|_| on), &now);
    reply(st, &r, c, &id).await
}

async fn favorite(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let Some(id) = target(st, &r, c) else {
        return Ok(Flow::Next(r));
    };
    let on = r.method == Method::POST;
    let op = if on { "favorite" } else { "unfavorite" };
    st.userdata
        .change(&c.user_id, &id, op, &|s| s.favorite = on);
    reply(st, &r, c, &id).await
}

async fn rating(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let Some(id) = target(st, &r, c) else {
        return Ok(Flow::Next(r));
    };
    let likes = if r.method == Method::DELETE {
        None
    } else {
        q_get(&r.query, "Likes").map(|v| v.eq_ignore_ascii_case("true"))
    };
    st.userdata
        .change(&c.user_id, &id, "rating", &|s| s.likes = likes);
    reply(st, &r, c, &id).await
}

async fn userdata(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let Some(id) = target(st, &r, c) else {
        return Ok(Flow::Next(r));
    };
    if r.method != Method::POST {
        let v = view::user_dto(&st.userdata, &c.user_id, &id, None, None);
        return done(ok_json(v));
    }
    let body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
    if !body.is_object() {
        return done((StatusCode::BAD_REQUEST, "invalid UserData body").into_response());
    }
    st.userdata
        .change(&c.user_id, &id, "update", &|s| rules::update_from(s, &body));
    reply(st, &r, c, &id).await
}
