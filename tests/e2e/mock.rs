//! Mock Jellyfin upstream (just enough of the API for the flows below).

use super::*;

pub(crate) const DATA_LEN: usize = 1000;

pub(crate) fn hex(n: u64) -> String {
    format!("{n:032x}")
}

pub(crate) struct Mock {
    pub(crate) name: &'static str,
    pub(crate) user: String,
    pub(crate) token: String,
    pub(crate) views: Vec<Value>,
    pub(crate) items: Vec<Value>,
    pub(crate) playstate: Mutex<Vec<String>>,
}

pub(crate) fn data() -> Vec<u8> {
    (0..DATA_LEN).map(|i| (i % 251) as u8).collect()
}

pub(crate) async fn mock_handler(State(m): State<Arc<Mock>>, req: Request) -> Response {
    let path = req.uri().path().to_ascii_lowercase();
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let headers = req.headers().clone();
    let method = req.method().clone();
    let body = axum::body::to_bytes(req.into_body(), 1 << 20)
        .await
        .unwrap();
    match segs.as_slice() {
        ["system", "info", "public"] => {
            return axum::Json(
                json!({"Version": "10.10.7", "ServerName": m.name, "Id": hex(0xaaaa)}),
            )
            .into_response()
        }
        ["users", "authenticatebyname"] => {
            let v: Value = serde_json::from_slice(&body).unwrap_or_default();
            if v["Username"] == "proxy" && v["Pw"] == "secret" {
                return axum::Json(json!({"AccessToken": m.token, "User": {"Id": m.user}}))
                    .into_response();
            }
            return StatusCode::UNAUTHORIZED.into_response();
        }
        _ => {}
    }
    if headers.get("x-emby-token").and_then(|v| v.to_str().ok()) != Some(m.token.as_str()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match segs.as_slice() {
        ["userviews"] => axum::Json(json!({"Items": m.views, "TotalRecordCount": m.views.len()})).into_response(),
        ["items"] => axum::Json(json!({"Items": m.items, "TotalRecordCount": m.items.len(), "StartIndex": 0})).into_response(),
        ["items", id, "playbackinfo"] => axum::Json(json!({
            "MediaSources": [{
                "Id": id,
                "SupportsDirectPlay": true,
                "TranscodingUrl": format!("/videos/{id}/master.m3u8?MediaSourceId={id}&api_key={}", m.token),
            }],
            "PlaySessionId": "abc"
        }))
        .into_response(),
        ["videos", _id, "stream"] | ["videos", _id, "stream.mkv"] => range_response(&headers),
        ["sessions", "playing", "progress"] if method == axum::http::Method::POST => {
            let v: Value = serde_json::from_slice(&body).unwrap_or_default();
            m.playstate.lock().unwrap().push(v["ItemId"].as_str().unwrap_or("").to_string());
            StatusCode::NO_CONTENT.into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

pub(crate) fn range_response(h: &HeaderMap) -> Response {
    let d = data();
    if let Some(r) = h
        .get("range")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
    {
        let (a, b) = r.split_once('-').unwrap();
        let a: usize = a.parse().unwrap();
        let b: usize = if b.is_empty() {
            DATA_LEN - 1
        } else {
            b.parse().unwrap()
        };
        return Response::builder()
            .status(206)
            .header("content-type", "video/x-matroska")
            .header("accept-ranges", "bytes")
            .header("content-range", format!("bytes {a}-{b}/{DATA_LEN}"))
            .header("content-length", (b - a + 1).to_string())
            .body(Body::from(d[a..=b].to_vec()))
            .unwrap();
    }
    Response::builder()
        .header("content-type", "video/x-matroska")
        .header("accept-ranges", "bytes")
        .body(Body::from(d))
        .unwrap()
}

pub(crate) async fn spawn_mock(m: Arc<Mock>, tls: Option<Arc<rustls::ServerConfig>>) -> u16 {
    let app = axum::Router::new().fallback(mock_handler).with_state(m);
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.set_nonblocking(true).unwrap();
    let port = l.local_addr().unwrap().port();
    match tls {
        Some(c) => {
            let rc = axum_server::tls_rustls::RustlsConfig::from_config(c);
            tokio::spawn(
                axum_server::tls_rustls::from_tcp_rustls(l, rc)
                    .unwrap()
                    .serve(app.into_make_service()),
            );
        }
        None => {
            tokio::spawn(
                axum_server::from_tcp(l)
                    .unwrap()
                    .serve(app.into_make_service()),
            );
        }
    }
    port
}
