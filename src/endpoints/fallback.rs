use super::{done, HandlerResult, Route};
use crate::handler;
use crate::proxy::forward as fwd;
use crate::proxy::{Client, Req};
use crate::Shared;

pub fn routes() -> Vec<Route> {
    vec![Route::auth("forward", handler!(forward)).at("**")]
}

async fn forward(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    done(fwd::generic(st, r, c).await?)
}
