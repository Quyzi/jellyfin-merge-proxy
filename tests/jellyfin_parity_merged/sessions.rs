//! Play-session routing and QuickConnect.

use super::*;

/// A PlaybackInfo response teaches the proxy which server owns a play
/// session; ActiveEncodings then stops the transcode only there (it used
/// to go to the primary, leaving the real transcode running), and pings
/// follow it. An unknown play session is sent to every server.
#[tokio::test]
async fn play_session_routes_encoding_stop_and_ping_to_its_owner() {
    let env = start("", |m, cfg| {
        let item = id(&["item", &cfg.name]);
        m.on_json(
            None,
            "Items",
            json!({"Items": [{"Id": item, "Name": format!("Movie {}", cfg.name), "Type": "Movie"}],
                   "TotalRecordCount": 1}),
        );
        m.on_json(
            Some(Method::POST),
            &format!("Items/{item}/PlaybackInfo"),
            json!({"MediaSources": [{"Id": item}], "PlaySessionId": format!("ps-{}", cfg.name)}),
        );
        m.on(
            Some(Method::DELETE),
            "Videos/ActiveEncodings",
            StatusCode::NO_CONTENT,
            "",
            Default::default(),
        );
        m.on(
            Some(Method::POST),
            "Sessions/Playing/Ping",
            StatusCode::NO_CONTENT,
            "",
            Default::default(),
        );
    })
    .await;
    let items = env.get("Items?Recursive=true&IncludeItemTypes=Movie").await;
    let s2_item = items["Items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["Name"] == "Movie s2")
        .unwrap()["Id"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, info) = env
        .req(
            "POST",
            &format!("Items/{s2_item}/PlaybackInfo"),
            Some(json!({})),
        )
        .await;
    assert!(st.is_success(), "{st} {info}");
    assert_eq!(info["PlaySessionId"], "ps-s2");

    let (st, _) = env
        .req(
            "DELETE",
            "Videos/ActiveEncodings?deviceId=dev1&playSessionId=ps-s2",
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let enc = |i| {
        env.calls(i, Method::DELETE)
            .iter()
            .filter(|p| p.starts_with("Videos/ActiveEncodings"))
            .count()
    };
    assert_eq!(
        (enc(0), enc(1)),
        (0, 1),
        "only the owner stops the transcode"
    );

    let (st, _) = env
        .req("POST", "Sessions/Playing/Ping?playSessionId=ps-s2", None)
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let ping = |i| {
        env.calls(i, Method::POST)
            .iter()
            .filter(|p| p.starts_with("Sessions/Playing/Ping"))
            .count()
    };
    assert_eq!((ping(0), ping(1)), (0, 1));
    let (st, _) = env
        .req("POST", "Sessions/Playing/Ping?playSessionId=nope", None)
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert_eq!(
        (ping(0), ping(1)),
        (0, 1),
        "unknown play session pings nobody"
    );

    let (st, _) = env
        .req(
            "DELETE",
            "Videos/ActiveEncodings?deviceId=dev1&playSessionId=nope",
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    assert_eq!(
        (enc(0), enc(1)),
        (1, 2),
        "unknown play session stops it everywhere"
    );

    for (m, p) in [
        ("DELETE", "Videos/ActiveEncodings?deviceId=dev1"),
        ("DELETE", "Videos/ActiveEncodings?playSessionId=ps-s2"),
        ("POST", "Sessions/Playing/Ping"),
    ] {
        let (st, _) = env.req(m, p, None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{m} {p}");
    }
}

/// QuickConnect is disabled; Jellyfin answers its other routes 401 with a
/// message (they used to be forwarded to the primary server).
#[tokio::test]
async fn quickconnect_routes_report_disabled() {
    let env = start("", |_, _| {}).await;
    for (m, p) in [
        ("POST", "QuickConnect/Initiate"),
        ("GET", "QuickConnect/Initiate"),
        ("GET", "QuickConnect/Connect?secret=abc"),
        ("POST", "QuickConnect/Authorize?code=123456"),
    ] {
        let (st, body) = env.req(m, p, None).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "{m} {p}");
        assert_eq!(body, "Quick connect is disabled", "{m} {p}");
    }
    let (st, body) = env.send("POST", "QuickConnect/Initiate", None, false).await;
    assert_eq!(
        (st, body),
        (StatusCode::UNAUTHORIZED, json!("Quick connect is disabled"))
    );
    for (m, p) in [
        ("GET", "QuickConnect/Connect"),
        ("POST", "QuickConnect/Authorize"),
    ] {
        let (st, _) = env.req(m, p, None).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{m} {p}: required parameter");
    }
    let (st, _) = env
        .send("POST", "QuickConnect/Authorize?code=1", None, false)
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert!(
        env.mock(0).calls().is_empty(),
        "nothing reached an upstream"
    );
}
