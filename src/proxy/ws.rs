//! WebSocket endpoint: a minimal keep-alive.

use super::*;

// ---------------------------------------------------------------------------
// WebSocket: minimal keep-alive so clients don't reconnect-loop.
// ---------------------------------------------------------------------------

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
    if info.get("token").and_then(|t| st.sessions.get(t)).is_none() {
        return err_resp(StatusCode::UNAUTHORIZED, "Unauthorized");
    }
    let (mut parts, _) = req.into_parts();
    use axum::extract::FromRequestParts;
    match WebSocketUpgrade::from_request_parts(&mut parts, &st).await {
        Ok(ws) => ws.on_upgrade(ws_loop),
        Err(e) => e.into_response(),
    }
}

pub(super) async fn ws_loop(mut sock: WebSocket) {
    let hello = json!({"MessageType": "ForceKeepAlive", "Data": 60}).to_string();
    if sock.send(Message::Text(hello.into())).await.is_err() {
        return;
    }
    while let Some(Ok(msg)) = sock.recv().await {
        if let Message::Text(t) = msg {
            if t.contains("KeepAlive") {
                let ka = json!({"MessageType": "KeepAlive"}).to_string();
                if sock.send(Message::Text(ka.into())).await.is_err() {
                    break;
                }
            }
        }
    }
}
