//! The current user, user settings and library views.

use super::{done, HandlerResult, Route};
use crate::dto;
use crate::handler;
use crate::proxy::{self, err_resp, no_content, ok_json, q_get, Client, Req};
use crate::types::normalize_hex_id;
use crate::Shared;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde_json::{json, Value};

pub fn routes() -> Vec<Route> {
    vec![
        Route::auth("current-user", handler!(me))
            .at("users/me")
            .at("users/{u}")
            .get(),
        Route::auth("unknown-user", handler!(unknown_user))
            .at("users/{id}")
            .get(),
        Route::auth("users", handler!(list)).at("users").get(),
        Route::auth("user-image", handler!(no_image))
            .at("users/{u}/images/**")
            .at("userimage")
            .get(),
        Route::auth("user-configuration", handler!(configuration))
            .at("users/configuration")
            .at("users/{u}/configuration")
            .post(),
        Route::auth("user-policy", handler!(policy))
            .at("users/{u}/policy")
            .post(),
        Route::auth("user-password", handler!(password))
            .at("users/password")
            .at("users/{u}/password")
            .post(),
        Route::auth("views", handler!(views))
            .at("users/{u}/views")
            .at("userviews")
            .get(),
        Route::auth("grouping-options", handler!(grouping))
            .at("users/{u}/groupingoptions")
            .at("userviews/groupingoptions"),
    ]
}

/// `users/me` is the caller; `users/{u}` is that user (any authenticated
/// caller may read any user, as `GetUserById` allows).
async fn me(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let id = normalize_hex_id(&r.segs[1]).unwrap_or_else(|| c.user_id.clone());
    done(ok_json(proxy::current_user(st, &id)))
}

async fn unknown_user(_st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done(err_resp(StatusCode::NOT_FOUND, "User not found"))
}

fn flag(r: &Req, key: &str) -> Option<bool> {
    q_get(&r.query, key).and_then(|v| v.to_ascii_lowercase().parse().ok())
}

async fn list(st: &Shared, r: Req, _c: &Client) -> HandlerResult {
    // Every proxy user is hidden and enabled.
    if flag(&r, "isHidden") == Some(false) || flag(&r, "isDisabled") == Some(true) {
        return done(ok_json(json!([])));
    }
    let mut users = st.authenticator.list();
    users.sort_by(|a, b| (a.name.to_lowercase(), &a.name).cmp(&(b.name.to_lowercase(), &b.name)));
    let v: Vec<Value> = users
        .iter()
        .map(|u| proxy::current_user(st, &u.id))
        .collect();
    done(ok_json(json!(v)))
}

/// The user a settings route targets: the `{u}` segment, else query
/// `userId`, else the caller. `None` when it names no proxy user.
fn target(st: &Shared, r: &Req, c: &Client) -> Option<String> {
    let raw =
        match r.segs.get(1).filter(|s| {
            !s.eq_ignore_ascii_case("configuration") && !s.eq_ignore_ascii_case("password")
        }) {
            Some(s) => Some(s.as_str()),
            None => q_get(&r.query, "userId"),
        };
    match raw {
        None => Some(c.user_id.clone()),
        Some(s) => normalize_hex_id(s).filter(|n| st.is_proxy_user(n)),
    }
}

async fn configuration(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let Some(t) = target(st, &r, c) else {
        return done(err_resp(StatusCode::NOT_FOUND, "Error processing request."));
    };
    if t != c.user_id {
        return done(err_resp(
            StatusCode::FORBIDDEN,
            "User configuration update not allowed",
        ));
    }
    let body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
    done(match st.local.settings.put(&t, &body) {
        Ok(()) => no_content(),
        Err(e) => err_resp(StatusCode::BAD_REQUEST, &e),
    })
}

/// Policy changes need elevation, which no proxy user has.
async fn policy(_st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done(StatusCode::FORBIDDEN.into_response())
}

async fn password(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let Some(t) = target(st, &r, c) else {
        return done(err_resp(StatusCode::NOT_FOUND, "Error processing request."));
    };
    let body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
    done(crate::local::password::change(st, c, &t, &body).await)
}

async fn no_image(_st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done(err_resp(StatusCode::NOT_FOUND, "no image"))
}

async fn views(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let v = proxy::views(st, c, false).await;
    let v = proxy::present_views(st, c, &r.query, v);
    let n = v.len();
    done(ok_json(dto::query_result(v, n, 0)))
}

async fn grouping(_st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done(ok_json(json!([])))
}
