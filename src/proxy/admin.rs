//! Metrics and debug endpoints.

use super::*;

pub(super) async fn metrics_handler(State(st): State<Shared>, req: Request) -> Response {
    if st.cfg().metrics.require_auth
        && session_from(&st, req.headers(), req.uri().query()).is_none()
    {
        return err_resp(StatusCode::UNAUTHORIZED, "Unauthorized");
    }
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        telemetry::render(),
    )
        .into_response()
}

/// Config-gated JSON dump of internal state. Never contains tokens or
/// passwords (sessions are shown with a short hash fingerprint).
pub(super) async fn debug_handler(State(st): State<Shared>, req: Request) -> Response {
    let cfg = st.cfg();
    let Some(sess) = session_from(&st, req.headers(), req.uri().query()) else {
        return err_resp(StatusCode::UNAUTHORIZED, "Unauthorized");
    };
    if !cfg.debug.users.is_empty()
        && !cfg
            .debug
            .users
            .iter()
            .any(|u| u.eq_ignore_ascii_case(&sess.user_name))
    {
        return err_resp(StatusCode::FORBIDDEN, "Forbidden");
    }
    let mut ups = vec![];
    let set = st.backends();
    for u in set.iter() {
        let mut v = serde_json::to_value(u.status().await).unwrap_or(Value::Null);
        if let Value::Object(o) = &mut v {
            o.insert("server_id".into(), json!(u.id().0));
            o.insert("in_flight".into(), json!(u.in_flight()));
        }
        ups.push(v);
    }
    let removed: Vec<String> = st
        .servers
        .names()
        .into_iter()
        .filter(|n| set.by_name(n).is_none())
        .collect();
    let settings = st.catalog.settings();
    let catalog = json!({
        "impl": st.catalog.name(),
        "entries": st.catalog.len(),
        "ttl_secs": settings.ttl.as_secs(),
        "max_entries": settings.max_entries,
    });
    let last_reload = st.last_reload.lock().unwrap().clone();
    ok_json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "git_sha": telemetry::git_sha(),
        "uptime_secs": st.started.elapsed().as_secs(),
        "server_id": st.server_id,
        "config": {
            "generation": st.generation.load(std::sync::atomic::Ordering::Relaxed),
            "source": st.source.as_ref().map(|s| s.describe()),
            "last_reload": last_reload.map(|(t, e)| json!({"at": t, "error": e})),
            "users": st.authenticator.list().into_iter().map(|u| u.name).collect::<Vec<_>>(),
            "merge": format!("{:?}", cfg.merge),
            "tls_listener": cfg.tls.is_some(),
        },
        "upstreams": ups,
        "removed_servers": removed,
        "id_map": {
            "total": st.ids.len(),
            "per_server": st.ids.sizes().into_iter().map(|(n, c)| json!({"server": n, "entries": c})).collect::<Vec<_>>(),
        },
        "caches": {
            "catalog": catalog,
            "display_prefs_entries": st.local.prefs.len(),
            "password_hashes": st.password_cache().len(),
        },
        "sessions": st.sessions.list(),
        "active_streams": telemetry::active_streams(),
        "architecture": {
            "endpoints": st.endpoints.names(),
            "rewriters": st.rewriters.names(),
            "reload_subscribers": st.hub.names(),
            "authenticator": st.authenticator.name(),
            "tls_provider": st.tls_provider.name(),
            "merge_strategy": merge::strategy_for(&cfg.merge).name(),
        },
    }))
}
