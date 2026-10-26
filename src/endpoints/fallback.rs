//! Everything else: forward to the backend owning the ids in the request.

use super::{done, HandlerResult, Route};
use crate::handler;
use crate::proxy::{self, Client, Req};
use crate::Shared;

pub fn routes() -> Vec<Route> {
    vec![Route::auth("forward", handler!(forward)).at("**")]
}

async fn forward(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    done(proxy::generic(st, r, c).await?)
}
