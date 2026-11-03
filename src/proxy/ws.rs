//! WebSocket endpoint: authenticates, upgrades and hands the socket to
//! [`crate::live::socket`].

use super::*;

pub(super) async fn ws_handler(State(st): State<Shared>, req: Request) -> Response {
    let query: Vec<(String, String)> = req
        .uri()
        .query()
        .map(|q| {
            form_urlencoded::parse(q.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default();
    let info = auth::client_auth(req.headers(), &query);
    let Some(session) = info.get("token").and_then(|t| st.sessions.get(t)) else {
        return err_resp(StatusCode::FORBIDDEN, "Error processing request.");
    };
    let (mut parts, _) = req.into_parts();
    use axum::extract::FromRequestParts;
    match WebSocketUpgrade::from_request_parts(&mut parts, &st).await {
        Ok(ws) => {
            let live = st.local.live.clone();
            let server_id = st.server_id.clone();
            ws.on_upgrade(move |sock| crate::live::socket::run(live, server_id, session, sock))
        }
        Err(e) => e.into_response(),
    }
}
