//! Server identity, branding, login and the web client.

use super::{done, HandlerResult, Route};
use crate::dto;
use crate::handler;
use crate::proxy::{self, ok_json, Client, Req};
use crate::Shared;
use axum::http::header;
use axum::response::{IntoResponse, Redirect};
use serde_json::json;

pub fn routes() -> Vec<Route> {
    vec![
        Route::public("root-redirect", handler!(root)).at(""),
        Route::public("system-info-public", handler!(info_public)).at("system/info/public"),
        Route::public("ping", handler!(ping)).at("system/ping"),
        Route::public("branding", handler!(branding)).at("branding/configuration"),
        Route::public("branding-css", handler!(css))
            .at("branding/css")
            .at("branding/css.css"),
        Route::public("quickconnect", handler!(quickconnect)).at("quickconnect/enabled"),
        Route::public("users-public", handler!(users_public)).at("users/public"),
        Route::public("login", handler!(login))
            .at("users/authenticatebyname")
            .post(),
        Route::public("web", handler!(web))
            .at("web/**")
            .at("favicon.ico")
            .at("robots.txt"),
        Route::auth("system-info", handler!(info)).at("system/info"),
        Route::auth("system-endpoint", handler!(endpoint)).at("system/endpoint"),
    ]
}

async fn root(_st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done(Redirect::temporary("/web/").into_response())
}

async fn info_public(st: &Shared, r: Req, _c: &Client) -> HandlerResult {
    let cfg = st.cfg();
    done(ok_json(dto::public_info(
        &cfg.proxy.name,
        &st.server_id,
        &cfg.proxy.reported_version,
        &proxy::host_address(&r.headers),
    )))
}

async fn info(st: &Shared, r: Req, _c: &Client) -> HandlerResult {
    let cfg = st.cfg();
    done(ok_json(dto::system_info(
        &cfg.proxy.name,
        &st.server_id,
        &cfg.proxy.reported_version,
        &proxy::host_address(&r.headers),
    )))
}

async fn ping(_st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done("Jellyfin Server".into_response())
}

async fn branding(_st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done(ok_json(dto::branding()))
}

async fn css(_st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done(([(header::CONTENT_TYPE, "text/css")], "").into_response())
}

async fn quickconnect(_st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done(ok_json(json!(false)))
}

async fn users_public(_st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done(ok_json(json!([])))
}

async fn login(st: &Shared, r: Req, _c: &Client) -> HandlerResult {
    done(proxy::login(st, &r).await)
}

async fn web(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    done(proxy::primary_forward(st, r, c).await?)
}

async fn endpoint(_st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done(ok_json(json!({"IsLocal": true, "IsInNetwork": true})))
}
