//! Endpoints clients probe that have no meaningful merged answer.

use super::{done, HandlerResult, Route};
use crate::handler;
use crate::proxy::{self, err_resp, Client, Req};
use crate::Shared;
use axum::http::StatusCode;
use axum::response::IntoResponse;

pub fn routes() -> Vec<Route> {
    vec![
        // `[AllowAnonymous]` in PluginsController; there are no plugins.
        Route::public("plugin-image", handler!(not_found))
            .at("plugins/*/*/image")
            .get(),
        // SyncPlay needs `SyncPlayAccess`, which no proxy user has; the
        // admin areas need elevation.
        Route::auth("forbidden", handler!(forbidden))
            .at("syncplay/**")
            .at("plugins")
            .at("plugins/**")
            .at("packages")
            .at("packages/**")
            .at("repositories")
            .at("repositories/**")
            .at("scheduledtasks")
            .at("scheduledtasks/**"),
        Route::auth("primary-passthrough", handler!(primary))
            .at("playback/bitratetest")
            .at("localization/**"),
    ]
}

async fn not_found(_st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done(err_resp(StatusCode::NOT_FOUND, "Not found"))
}

async fn forbidden(_st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done(StatusCode::FORBIDDEN.into_response())
}

async fn primary(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    done(proxy::primary_forward(st, r, c).await?)
}
