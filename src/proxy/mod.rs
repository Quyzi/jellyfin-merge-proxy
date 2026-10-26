pub mod auth;
pub mod catalog;
pub mod forward;
pub mod handlers;
pub mod list;
pub mod response;

use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use axum::routing::any;
use axum::Router;

use crate::auth::Session;
use crate::telemetry;
use crate::Shared;

use response::err_resp;

const MAX_BODY: usize = 64 * 1024 * 1024;

pub struct Req {
    pub method: Method,
    pub segs: Vec<String>,
    pub query: Vec<(String, String)>,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub raw_path: String,
}

#[derive(Clone)]
pub struct Client {
    pub token: String,
    pub user_id: String,
    pub session: Option<Session>,
}

pub fn router(st: Shared) -> Router {
    let cfg = st.cfg();
    let mut r = Router::new()
        .route("/socket", any(handlers::ws_handler))
        .route("/embywebsocket", any(handlers::ws_handler));
    if cfg.metrics.enabled && cfg.metrics.listen.is_none() {
        r = r.route(
            &cfg.metrics.path,
            axum::routing::get(handlers::metrics_handler),
        );
    }
    if cfg.debug.enabled {
        r = r.route(&cfg.debug.path, axum::routing::get(handlers::debug_handler));
    }
    r.fallback(dispatch)
        .layer(tower_http::cors::CorsLayer::very_permissive())
        .layer(
            tower_http::trace::TraceLayer::new_for_http()
                .make_span_with(telemetry::make_span)
                .on_request(())
                .on_response(
                    tower_http::trace::DefaultOnResponse::new()
                        .level(tracing::Level::DEBUG)
                        .latency_unit(tower_http::LatencyUnit::Millis),
                )
                .on_failure(tower_http::trace::DefaultOnFailure::new().level(tracing::Level::WARN)),
        )
        .layer(axum::middleware::from_fn(telemetry::http_layer))
        .with_state(st)
}

pub fn metrics_router(st: Shared) -> Router {
    let path = st.cfg().metrics.path.clone();
    Router::new()
        .route(&path, axum::routing::get(handlers::metrics_handler))
        .with_state(st)
}

async fn dispatch(State(st): State<Shared>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let raw_path = parts.uri.path().to_string();
    let mut segs: Vec<String> = raw_path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if segs
        .first()
        .is_some_and(|s| s.eq_ignore_ascii_case("emby") || s.eq_ignore_ascii_case("mediabrowser"))
    {
        segs.remove(0);
    }
    let query: Vec<(String, String)> = parts
        .uri
        .query()
        .map(|q| {
            form_urlencoded::parse(q.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default();
    let body = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(_) => {
            telemetry::http_error("payload_too_large");
            return err_resp(StatusCode::PAYLOAD_TOO_LARGE, "body too large");
        }
    };
    let r = Req {
        method: parts.method,
        segs,
        query,
        headers: parts.headers,
        body,
        raw_path,
    };
    let method = r.method.clone();
    let route = telemetry::route_template(&r.raw_path);
    let resp = match st.endpoints.dispatch(&st, r).await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(%method, %route, "request failed: {e:#}");
            telemetry::http_error("upstream_error");
            err_resp(StatusCode::BAD_GATEWAY, "upstream error")
        }
    };
    match resp.status() {
        StatusCode::UNAUTHORIZED => telemetry::http_error("unauthorized"),
        StatusCode::NOT_FOUND => telemetry::http_error("not_found"),
        StatusCode::SERVICE_UNAVAILABLE => telemetry::http_error("upstream_unavailable"),
        _ => {}
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use list::ensure_field;

    fn q(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn ensure_field_adds_or_appends() {
        let mut a = q(&[("Limit", "5")]);
        ensure_field(&mut a, "ProviderIds");
        assert_eq!(a, q(&[("Limit", "5"), ("Fields", "ProviderIds")]));
        let mut b = q(&[("fields", "PrimaryImageAspectRatio,Path")]);
        ensure_field(&mut b, "ProviderIds");
        assert_eq!(
            b,
            q(&[("fields", "PrimaryImageAspectRatio,Path,ProviderIds")])
        );
        let mut c = q(&[("Fields", "providerids")]);
        ensure_field(&mut c, "ProviderIds");
        assert_eq!(c, q(&[("Fields", "providerids")]));
        let mut d = q(&[("Fields", "")]);
        ensure_field(&mut d, "ProviderIds");
        assert_eq!(d, q(&[("Fields", "ProviderIds")]));
    }

    #[test]
    fn trailing_slash_is_kept() {
        let path_with_query_slash = |s: &[String], q: &[(String, String)], slash: bool| {
            let mut p = s.join("/");
            if slash && !p.is_empty() {
                p.push('/');
            }
            if !q.is_empty() {
                p.push('?');
                p.push_str(&crate::rewrite::encode_pairs(q));
            }
            p
        };
        let segs = vec!["web".to_string()];
        assert_eq!(path_with_query_slash(&segs, &[], true), "web/");
        assert_eq!(path_with_query_slash(&segs, &[], false), "web");
        assert_eq!(path_with_query_slash(&[], &[], true), "");
        assert_eq!(
            path_with_query_slash(&segs, &q(&[("a", "1")]), true),
            "web/?a=1"
        );
    }
}
