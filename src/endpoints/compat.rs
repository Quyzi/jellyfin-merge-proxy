//! Endpoints clients probe that have no meaningful merged answer.

use super::{done, HandlerResult, Route};
use crate::handler;
use crate::proxy::forward;
use crate::proxy::response::ok_json;
use crate::proxy::{Client, Req};
use crate::Shared;
use serde_json::json;

pub fn routes() -> Vec<Route> {
    vec![
        Route::auth("syncplay", handler!(empty_list)).at("syncplay/**"),
        Route::auth("admin-lists", handler!(empty_list))
            .at("plugins")
            .at("packages")
            .at("scheduledtasks")
            .get(),
        Route::auth("primary-passthrough", handler!(primary))
            .at("playback/bitratetest")
            .at("localization/**"),
    ]
}

async fn empty_list(_st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done(ok_json(json!([])))
}

async fn primary(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    done(forward::primary_forward(st, r, c).await?)
}
