//! Server identity, branding, login and the web client.

use super::{done, HandlerResult, Route};
use crate::dto;
use crate::handler;
use crate::proxy::{self, ok_json, Client, Req};
use crate::Shared;
use axum::http::{header, Method, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
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
        Route::public("quickconnect-initiate", handler!(quickconnect_disabled))
            .at("quickconnect/initiate")
            .methods(&[Method::GET, Method::POST]),
        Route::public("quickconnect-connect", handler!(quickconnect_connect))
            .at("quickconnect/connect")
            .get(),
        Route::auth("quickconnect-authorize", handler!(quickconnect_authorize))
            .at("quickconnect/authorize")
            .post(),
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

/// Quick Connect stays disabled (`Enabled` says so); Jellyfin answers its
/// other routes 401 in that case (`QuickConnectController.cs`).
fn quickconnect_off() -> Response {
    proxy::err_resp(StatusCode::UNAUTHORIZED, "Quick connect is disabled")
}

async fn quickconnect_disabled(_st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done(quickconnect_off())
}

/// `secret` is a required query parameter.
async fn quickconnect_connect(_st: &Shared, r: Req, _c: &Client) -> HandlerResult {
    if proxy::q_get(&r.query, "secret").is_none() {
        return done(missing_param());
    }
    done(quickconnect_off())
}

/// `code` is a required query parameter.
async fn quickconnect_authorize(_st: &Shared, r: Req, _c: &Client) -> HandlerResult {
    if proxy::q_get(&r.query, "code").is_none() {
        return done(missing_param());
    }
    done(quickconnect_off())
}

fn missing_param() -> Response {
    proxy::err_resp(
        StatusCode::BAD_REQUEST,
        "One or more validation errors occurred.",
    )
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
