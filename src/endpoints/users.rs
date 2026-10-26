//! The current user, user settings and library views.

use super::{done, HandlerResult, Route};
use crate::dto;
use crate::handler;
use crate::proxy::auth;
use crate::proxy::catalog;
use crate::proxy::response::{err_resp, no_content, ok_json};
use crate::proxy::{Client, Req};
use crate::Shared;
use axum::http::StatusCode;
use serde_json::json;

pub fn routes() -> Vec<Route> {
    vec![
        Route::auth("current-user", handler!(me))
            .at("users/me")
            .at("users/{u}")
            .get(),
        Route::auth("users", handler!(list)).at("users").get(),
        Route::auth("user-image", handler!(no_image))
            .at("users/{u}/images/**")
            .at("userimage")
            .get(),
        Route::auth("user-settings", handler!(accept))
            .at("users/{u}/configuration")
            .at("users/configuration")
            .at("users/{u}/policy")
            .at("users/{u}/password")
            .at("users/password"),
        Route::auth("views", handler!(views))
            .at("users/{u}/views")
            .at("userviews")
            .get(),
        Route::auth("grouping-options", handler!(grouping))
            .at("users/{u}/groupingoptions")
            .at("userviews/groupingoptions"),
    ]
}

async fn me(st: &Shared, _r: Req, c: &Client) -> HandlerResult {
    done(ok_json(auth::current_user(st, c)))
}

async fn list(st: &Shared, _r: Req, c: &Client) -> HandlerResult {
    done(ok_json(json!([auth::current_user(st, c)])))
}

async fn no_image(_st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done(err_resp(StatusCode::NOT_FOUND, "no image"))
}

async fn accept(_st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done(no_content())
}

async fn views(st: &Shared, _r: Req, c: &Client) -> HandlerResult {
    let v = catalog::views(st, c, false).await;
    let n = v.len();
    done(ok_json(dto::query_result(v, n, 0)))
}

async fn grouping(_st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done(ok_json(json!([])))
}
