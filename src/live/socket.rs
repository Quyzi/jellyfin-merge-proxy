//! One client WebSocket: keep-alive and the `Sessions` push.
//!
//! Follows `WebSocketManager` / `SessionWebSocketListener` /
//! `SessionInfoWebSocketListener` of Jellyfin. Holds the live registry only,
//! not the application state.

use super::{envelope, LiveSessions};
use crate::auth::Session;
use axum::extract::ws::{Message, WebSocket};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::RecvError;

/// How often keep-alive is checked (`SessionWebSocketListener`).
const CHECK_INTERVAL: Duration = Duration::from_secs(12);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepAlive {
    None,
    /// Ask the client to send a keep-alive.
    Probe,
    /// The client stopped sending them.
    Lost,
}

/// What to do about a socket that last sent a keep-alive `elapsed` ago.
pub fn keepalive_action(elapsed: Duration) -> KeepAlive {
    if elapsed >= Duration::from_secs(60) {
        KeepAlive::Lost
    } else if elapsed >= Duration::from_secs(45) {
        KeepAlive::Probe
    } else {
        KeepAlive::None
    }
}

async fn send(socket: &mut WebSocket, text: String) -> bool {
    socket.send(Message::Text(text.into())).await.is_ok()
}

/// `SessionsStart` data: `"<initialDelayMs>,<intervalMs>"`.
fn parse_interval(data: &Value) -> Option<Duration> {
    let (a, b) = data.as_str()?.split_once(',')?;
    a.trim().parse::<u64>().ok()?;
    Some(Duration::from_millis(b.trim().parse::<u64>().ok()?))
}

pub async fn run(
    live: Arc<LiveSessions>,
    server_id: String,
    session: Session,
    mut socket: WebSocket,
) {
    live.touch(&session);
    let Some((sid, mut rx)) = live.attach(&session.session_id) else {
        return;
    };
    let mut changed = live.subscribe();
    let mut last_keepalive = Instant::now();
    let mut probing = true;
    let mut interval: Option<Duration> = None;
    let mut last_sent: Option<Instant> = None;
    let mut tick = tokio::time::interval(CHECK_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await;

    if !send(&mut socket, envelope("ForceKeepAlive", Some(json!(60)))).await {
        live.detach(&session.session_id, sid);
        return;
    }
    'run: loop {
        let mut push = false;
        tokio::select! {
            frame = socket.recv() => match frame {
                Some(Ok(Message::Text(t))) => {
                    let Ok(m) = serde_json::from_str::<Value>(&t) else { continue };
                    match m.get("MessageType").and_then(Value::as_str) {
                        Some("KeepAlive") => {
                            last_keepalive = Instant::now();
                            probing = true;
                            if !send(&mut socket, envelope("KeepAlive", None)).await {
                                break 'run;
                            }
                        }
                        Some("SessionsStart") => {
                            if let Some(i) = m.get("Data").and_then(parse_interval) {
                                interval = Some(i);
                                last_sent = None;
                            }
                        }
                        Some("SessionsStop") => interval = None,
                        _ => {}
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break 'run,
                Some(Ok(_)) => {}
            },
            out = rx.recv() => match out {
                Some(text) => {
                    if !send(&mut socket, text).await {
                        break 'run;
                    }
                }
                None => {
                    let _ = socket.send(Message::Close(None)).await;
                    break 'run;
                }
            },
            ev = changed.recv() => match ev {
                Ok((u, force)) if u == session.user_id => {
                    push = interval.is_some_and(|i| {
                        force || last_sent.is_none_or(|t| t.elapsed() >= i)
                    });
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => push = interval.is_some(),
                Err(RecvError::Closed) => break 'run,
            },
            _ = tick.tick() => match keepalive_action(last_keepalive.elapsed()) {
                KeepAlive::Probe if probing => {
                    if !send(&mut socket, envelope("ForceKeepAlive", Some(json!(60)))).await {
                        break 'run;
                    }
                }
                KeepAlive::Lost => probing = false,
                _ => {}
            },
        }
        if push {
            if let Ok(list) = live.visible(&session.user_id, &[], &server_id) {
                last_sent = Some(Instant::now());
                if !send(&mut socket, envelope("Sessions", Some(Value::Array(list)))).await {
                    break 'run;
                }
            }
        }
    }
    live.detach(&session.session_id, sid);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The keep-alive probe fired too early or too late, or a lost client
    /// was never noticed.
    #[test]
    fn keepalive_thresholds_match_jellyfin() {
        let s = Duration::from_secs;
        assert_eq!(keepalive_action(s(44)), KeepAlive::None);
        assert_eq!(keepalive_action(s(45)), KeepAlive::Probe);
        assert_eq!(keepalive_action(s(59)), KeepAlive::Probe);
        assert_eq!(keepalive_action(s(60)), KeepAlive::Lost);
    }

    /// `SessionsStart` data that is not `delay,interval` started a push
    /// subscription anyway.
    #[test]
    fn sessions_start_data_is_validated() {
        assert_eq!(
            parse_interval(&json!("0,1500")),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(parse_interval(&json!("1500")), None);
        assert_eq!(parse_interval(&json!("a,b")), None);
        assert_eq!(parse_interval(&json!(5)), None);
    }
}
