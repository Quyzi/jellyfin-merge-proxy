//! Remote control and pushed notifications.

use super::*;

/// Play and playstate commands must reach the target's socket with the
/// controlling user; unknown sessions are 404 and other users' sessions
/// stay private.
#[tokio::test]
async fn remote_commands_reach_the_target_socket() {
    let (env, _f) = start("[[user]]\nname = \"you\"\npassword = \"pw\"\n").await;
    let a = env.login("me", "pw", "devA").await;
    let b = env.login("me", "pw", "devB").await;
    let you = env.login("you", "pw", "devY").await;
    let mut ws = env.socket(&a.token).await;
    let movie = hex();
    let (st, _) = env
        .req(
            &b.token,
            "devB",
            "POST",
            &format!(
                "Sessions/{}/Playing?playCommand=PlayNow&itemIds={movie}&startPositionTicks=5",
                a.session_id
            ),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let play = recv_kind(&mut ws, "Play").await;
    assert_eq!(play["Data"]["ItemIds"], json!([movie]), "{play}");
    assert_eq!(play["Data"]["ControllingUserId"], a.user_id.as_str());
    assert_eq!(play["Data"]["PlayCommand"], "PlayNow");
    assert_eq!(play["Data"]["StartPositionTicks"], 5);

    let (st, _) = env
        .req(
            &b.token,
            "devB",
            "POST",
            &format!("Sessions/{}/Playing/Pause", a.session_id),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let ps = recv_kind(&mut ws, "Playstate").await;
    assert_eq!(ps["Data"]["Command"], "Pause", "{ps}");

    let (st, _) = env
        .req(
            &b.token,
            "devB",
            "POST",
            &format!("Sessions/{}/Playing/Bogus", a.session_id),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = env
        .req(
            &b.token,
            "devB",
            "POST",
            &format!("Sessions/{}/Playing/Pause", hex()),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = env
        .req(
            &you.token,
            "devY",
            "POST",
            &format!("Sessions/{}/Playing/Pause", a.session_id),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let (st, _) = env
        .req(
            &b.token,
            "devB",
            "POST",
            &format!("Sessions/{}/Message", a.session_id),
            Some(json!({"Text": "hi", "TimeoutMs": 3000})),
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    let msg = recv_kind(&mut ws, "GeneralCommand").await;
    assert_eq!(msg["Data"]["Name"], "DisplayMessage");
    assert_eq!(msg["Data"]["Arguments"]["Header"], "Message from Server");
    assert_eq!(msg["Data"]["Arguments"]["TimeoutMs"], "3000");
}

/// Marking an item played on one device did not tell the user's other
/// devices, so their home rows stayed stale.
#[tokio::test]
async fn played_state_is_pushed_to_the_users_other_sockets() {
    let (env, _f) = start("").await;
    let a = env.login("me", "pw", "devA").await;
    let b = env.login("me", "pw", "devB").await;
    let mut ws = env.socket(&a.token).await;
    let movie = hex();
    let (st, _) = env
        .req(
            &b.token,
            "devB",
            "POST",
            &format!("UserPlayedItems/{movie}"),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let msg = recv_kind(&mut ws, "UserDataChanged").await;
    let list = &msg["Data"]["UserDataList"];
    assert_eq!(list[0]["ItemId"], movie.as_str(), "{msg}");
    assert_eq!(list[0]["Played"], true);
    assert_eq!(msg["Data"]["UserId"], a.user_id.as_str());
}

/// Subscribers to `Sessions` saw nothing when another device started
/// playing, since the push was never generated.
#[tokio::test]
async fn sessions_push_shows_what_another_device_plays() {
    let (env, _f) = start("").await;
    let a = env.login("me", "pw", "devA").await;
    let b = env.login("me", "pw", "devB").await;
    let movie = a_movie(&env, &a, "devA").await;
    let mut ws = env.socket(&a.token).await;
    send(
        &mut ws,
        json!({"MessageType": "SessionsStart", "Data": "0,1000"}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (st, _) = env
        .req(
            &b.token,
            "devB",
            "POST",
            "Sessions/Playing",
            Some(json!({"ItemId": movie, "PositionTicks": 0, "IsPaused": true})),
        )
        .await;
    assert!(st.is_success(), "{st}");
    let mut playing = None;
    for _ in 0..5 {
        let m = recv_kind(&mut ws, "Sessions").await;
        let b_session = m["Data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["Id"] == b.session_id.as_str())
            .cloned();
        if let Some(s) = b_session.filter(|s| s.get("NowPlayingItem").is_some()) {
            playing = Some(s);
            break;
        }
    }
    let s = playing.expect("a Sessions push with B's NowPlayingItem");
    assert_eq!(s["NowPlayingItem"]["Id"], movie.as_str());
    assert_eq!(s["PlayState"]["IsPaused"], true);
    assert!(s.get("LastPausedDate").is_some(), "{s}");
}

/// An index rebuild that adds a movie must tell connected users with
/// `LibraryChanged`, as Jellyfin does after a library scan.
#[tokio::test]
async fn new_catalog_items_are_pushed_as_library_changed() {
    let (env, _f) = start("[index]\nenabled = true\n").await;
    let a = env.login("me", "pw", "devA").await;
    let mut ws = env.socket(&a.token).await;
    // Wait for the first build, then let the servers gain a movie.
    tokio::time::timeout(Duration::from_secs(5), async {
        while env.state.index.latest().is_none() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("first index build");
    SECOND_MOVIE.store(true, std::sync::atomic::Ordering::Relaxed);
    env.state.index.poke();
    let msg = recv_kind(&mut ws, "LibraryChanged").await;
    let added = msg["Data"]["ItemsAdded"].as_array().unwrap();
    assert!(!added.is_empty(), "{msg}");
    assert_eq!(msg["Data"]["IsEmpty"], false);
    assert!(msg["Data"]["ItemsRemoved"].as_array().unwrap().is_empty());
}

/// `PlayNow` of a playlist id sent the playlist's entries to the target
/// session whoever the controller was, so a private or un-shared playlist
/// could be played by anyone who knew its id. It plays only for users who
/// can see the playlist, and stops playing once the owner unshares it.
#[tokio::test]
async fn play_now_of_a_playlist_needs_visibility() {
    let (env, _f) = start("[[user]]\nname = \"you\"\npassword = \"pw\"\n").await;
    let me = env.login("me", "pw", "devA").await;
    let you = env.login("you", "pw", "devY").await;
    let movie = a_movie(&env, &me, "devA").await;
    let (st, v) = env
        .req(
            &me.token,
            "devA",
            "POST",
            "Playlists",
            Some(json!({"Name": "Private", "Ids": [movie], "IsPublic": false})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let pl = v["Id"].as_str().unwrap().to_string();
    let mut ws = env.socket(&you.token).await;
    let play = |token: &str, device: &str, session: &str| {
        let path = format!("Sessions/{session}/Playing?playCommand=PlayNow&itemIds={pl}");
        let (token, device) = (token.to_string(), device.to_string());
        let env = &env;
        async move { env.req(&token, &device, "POST", &path, None).await.0 }
    };

    assert_eq!(
        play(&you.token, "devY", &you.session_id).await,
        StatusCode::NO_CONTENT
    );
    let msg = recv_kind(&mut ws, "Play").await;
    assert_eq!(msg["Data"]["ItemIds"], json!([]), "private playlist: {msg}");

    let (st, _) = env
        .req(
            &me.token,
            "devA",
            "POST",
            &format!("Playlists/{pl}/Users/{}", you.user_id),
            Some(json!({"CanEdit": false})),
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    play(&you.token, "devY", &you.session_id).await;
    let msg = recv_kind(&mut ws, "Play").await;
    assert_eq!(
        msg["Data"]["ItemIds"],
        json!([movie]),
        "shared playlist: {msg}"
    );

    let (st, _) = env
        .req(
            &me.token,
            "devA",
            "DELETE",
            &format!("Playlists/{pl}/Users/{}", you.user_id),
            None,
        )
        .await;
    assert_eq!(st, StatusCode::NO_CONTENT);
    play(&you.token, "devY", &you.session_id).await;
    let msg = recv_kind(&mut ws, "Play").await;
    assert_eq!(msg["Data"]["ItemIds"], json!([]), "after unshare: {msg}");
}
