//! Play-session routing: `Videos/ActiveEncodings` and
//! `Sessions/Playing/Ping` carry a `PlaySessionId`, not an item id.

use super::*;

/// `DELETE Videos/ActiveEncodings`: stop the transcode on the server that
/// started it (every live server when the play session is unknown, e.g.
/// after a restart). Always 204, as Jellyfin answers.
pub async fn stop_encoding(st: &Shared, r: Req, c: &Client) -> Result<Response> {
    let (Some(_), Some(psid)) = (
        q_get(&r.query, "deviceId"),
        q_get(&r.query, "playSessionId"),
    ) else {
        return Ok(bad_request());
    };
    let targets: Vec<ServerId> = match st.local.play_sessions.owner(psid) {
        Some(s) => vec![s],
        None => st
            .backends()
            .iter()
            .filter(|b| !b.is_down())
            .map(|b| b.id())
            .collect(),
    };
    let sends = targets
        .into_iter()
        .map(|s| forward(st, s, copy_req(&r), c, None));
    for res in join_all(sends).await {
        if let Err(e) = res {
            tracing::warn!("stopping the transcode failed: {e:#}");
        }
    }
    Ok(no_content())
}

/// `POST Sessions/Playing/Ping`: keep the transcode of this play session
/// alive on its server. 204 for sessions the proxy does not know.
pub async fn playing_ping(st: &Shared, r: Req, c: &Client) -> Result<Response> {
    let Some(psid) = q_get(&r.query, "playSessionId") else {
        return Ok(bad_request());
    };
    if let Some(s) = st.local.play_sessions.owner(psid) {
        if let Err(e) = forward(st, s, r, c, None).await {
            tracing::warn!("play session ping failed: {e:#}");
        }
    }
    Ok(no_content())
}
