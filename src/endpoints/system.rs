//! Server identity, branding, login and the web client.

use super::{done, HandlerResult, Route};
use crate::backend;
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

/// Only used before any upstream has ever reported a version this process.
const FALLBACK_VERSION: &str = "10.10.7";

/// Pin if non-empty; else live max when any upstream reports; else sticky;
/// else `FALLBACK_VERSION` (never stored as sticky). Live always wins over
/// a higher sticky so a removed/downgraded upstream can drop the reported
/// version.
fn pick_reported_version<'a>(
    pinned: &str,
    live: impl IntoIterator<Item = &'a str>,
    sticky: Option<&str>,
) -> (String, Option<String>) {
    let pinned = pinned.trim();
    if !pinned.is_empty() {
        return (pinned.to_string(), sticky.map(str::to_string));
    }
    if let Some(l) = live
        .into_iter()
        .max_by(|a, b| backend::jellyfin::cmp_version(a, b))
    {
        let s = l.to_string();
        return (s.clone(), Some(s));
    }
    match sticky {
        Some(s) => (s.to_string(), Some(s.to_string())),
        None => (FALLBACK_VERSION.to_string(), None),
    }
}

/// Reads cached `BackendStatus.version` only — never fans out live.
async fn resolve_reported_version(st: &Shared) -> String {
    let pinned = st.cfg().proxy.reported_version.clone();
    let mut live = Vec::new();
    for b in st.backends().iter() {
        if let Some(v) = b.status().await.version {
            live.push(v);
        }
    }
    let mut slot = st.last_auto_version.lock().unwrap();
    let (out, new_sticky) =
        pick_reported_version(&pinned, live.iter().map(|s| s.as_str()), slot.as_deref());
    if pinned.trim().is_empty() {
        *slot = new_sticky;
    }
    out
}

async fn info_public(st: &Shared, r: Req, _c: &Client) -> HandlerResult {
    let cfg = st.cfg();
    let version = resolve_reported_version(st).await;
    done(ok_json(dto::public_info(
        &cfg.proxy.name,
        &st.server_id,
        &version,
        &proxy::host_address(&r.headers),
    )))
}

async fn info(st: &Shared, r: Req, _c: &Client) -> HandlerResult {
    let cfg = st.cfg();
    let version = resolve_reported_version(st).await;
    done(ok_json(dto::system_info(
        &cfg.proxy.name,
        &st.server_id,
        &version,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_pin_wins() {
        let (v, sticky) = pick_reported_version("10.9.0", ["10.11.0", "10.10.7"], Some("10.11.0"));
        assert_eq!(v, "10.9.0");
        assert_eq!(sticky.as_deref(), Some("10.11.0"));
    }

    #[test]
    fn pick_blank_takes_max_and_sticks() {
        let (v, sticky) = pick_reported_version("", ["10.10.7", "10.11.2", "10.11.5"], None);
        assert_eq!(v, "10.11.5");
        assert_eq!(sticky.as_deref(), Some("10.11.5"));
    }

    #[test]
    fn pick_blank_keeps_sticky_when_live_empty() {
        let (v, sticky) = pick_reported_version("", None, Some("10.11.2"));
        assert_eq!(v, "10.11.2");
        assert_eq!(sticky.as_deref(), Some("10.11.2"));
    }

    #[test]
    fn pick_live_drops_higher_sticky() {
        let (v, sticky) = pick_reported_version("", ["10.10.7"], Some("10.11.5"));
        assert_eq!(v, "10.10.7");
        assert_eq!(sticky.as_deref(), Some("10.10.7"));
    }

    #[test]
    fn pick_blank_no_signal_falls_back_without_sticking() {
        let (v, sticky) = pick_reported_version("", None, None);
        assert_eq!(v, FALLBACK_VERSION);
        assert_eq!(sticky, None);
    }
}
