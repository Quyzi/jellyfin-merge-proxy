//! Live TV across servers, and the Live TV routes proxy users may not use.

use super::*;

fn items_of(v: &Value) -> Vec<String> {
    v["Items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["Name"].as_str().unwrap().to_string())
        .collect()
}

/// Live TV lists only ever reached the primary server. Each list route must
/// ask every server (on its Jellyfin path) and merge their items.
#[tokio::test]
async fn live_tv_lists_merge_every_server() {
    const ROUTES: &[(&str, &str)] = &[
        ("LiveTv/Channels", "LiveTv/Channels"),
        ("LiveTv/Programs", "LiveTv/Programs"),
        ("LiveTv/Programs/Recommended", "LiveTv/Programs/Recommended"),
        ("LiveTv/Recordings", "LiveTv/Recordings"),
        ("LiveTv/Recordings/Folders", "LiveTv/Recordings/Folders"),
        ("LiveTv/Timers", "LiveTv/Timers"),
        ("LiveTv/SeriesTimers", "LiveTv/SeriesTimers"),
    ];
    let env = start("", |m, cfg| {
        for (_, up) in ROUTES {
            m.on_json(
                Some(Method::GET),
                up,
                json!({"Items": [{"Id": id(&[up, &cfg.name]), "Name": format!("{up} {}", cfg.name)}],
                       "TotalRecordCount": 1}),
            );
        }
    })
    .await;
    for (route, up) in ROUTES {
        let v = env.get(route).await;
        let mut names = items_of(&v);
        names.sort();
        assert_eq!(
            names,
            [format!("{up} s1"), format!("{up} s2")],
            "{route}: {v}"
        );
        assert_eq!(v["TotalRecordCount"], 2, "{route}: {v}");
    }
}

/// Programs from several servers must interleave by start time, not be
/// listed server by server.
#[tokio::test]
async fn live_tv_programs_sort_by_start_date() {
    let env = start("", |m, cfg| {
        let starts: &[&str] = if cfg.name == "s1" {
            &[
                "2026-11-03T10:00:00.0000000Z",
                "2026-11-03T12:00:00.0000000Z",
            ]
        } else {
            &["2026-11-03T11:00:00.0000000Z"]
        };
        let items: Vec<Value> = starts
            .iter()
            .map(|s| json!({"Id": id(&["p", &cfg.name, s]), "Name": *s, "StartDate": *s}))
            .collect();
        m.on_json(
            Some(Method::GET),
            "LiveTv/Programs",
            json!({"Items": items, "TotalRecordCount": items.len()}),
        );
    })
    .await;
    let v = env.get("LiveTv/Programs").await;
    assert_eq!(
        items_of(&v),
        [
            "2026-11-03T10:00:00.0000000Z",
            "2026-11-03T11:00:00.0000000Z",
            "2026-11-03T12:00:00.0000000Z"
        ],
        "{v}"
    );
}

/// POST LiveTv/Programs: channel ids belong to one server each, so only the
/// server owning a requested channel is asked; with no channel filter every
/// server is, and the merged page is sorted, paged, with summed totals.
#[tokio::test]
async fn live_tv_program_query_goes_to_the_servers_owning_the_channels() {
    let env = start("", |m, cfg| {
        m.on_json(
            Some(Method::GET),
            "LiveTv/Channels",
            json!({"Items": [{"Id": id(&["ch", &cfg.name]), "Name": format!("ch {}", cfg.name)}],
                   "TotalRecordCount": 1}),
        );
        let starts: &[&str] = if cfg.name == "s1" {
            &["2026-11-03T10:00:00Z", "2026-11-03T12:00:00Z"]
        } else {
            &["2026-11-03T11:00:00Z", "2026-11-03T13:00:00Z"]
        };
        let items: Vec<Value> = starts
            .iter()
            .map(|s| json!({"Id": id(&["q", &cfg.name, s]), "Name": *s, "StartDate": *s}))
            .collect();
        m.on_json(
            Some(Method::POST),
            "LiveTv/Programs",
            json!({"Items": items, "TotalRecordCount": 40}),
        );
    })
    .await;
    let channels = env.get("LiveTv/Channels").await;
    let ch_s1 = channels["Items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["Name"] == "ch s1")
        .unwrap()["Id"]
        .as_str()
        .unwrap()
        .to_string();
    let posts = |i| env.calls(i, Method::POST).len();

    let (st, v) = env
        .req(
            "POST",
            "LiveTv/Programs",
            Some(json!({"ChannelIds": [ch_s1]})),
        )
        .await;
    assert!(st.is_success(), "{st} {v}");
    assert_eq!(
        (posts(0), posts(1)),
        (1, 0),
        "only the channel's server is asked"
    );
    assert_eq!(items_of(&v).len(), 2, "{v}");

    let (st, v) = env
        .req(
            "POST",
            "LiveTv/Programs",
            Some(json!({"StartIndex": 1, "Limit": 2})),
        )
        .await;
    assert!(st.is_success(), "{st} {v}");
    assert_eq!((posts(0), posts(1)), (2, 1));
    assert_eq!(
        items_of(&v),
        ["2026-11-03T11:00:00Z", "2026-11-03T12:00:00Z"],
        "page 1..3 of the merged, sorted list: {v}"
    );
    assert_eq!(v["TotalRecordCount"], 80, "{v}");
}

/// LiveTv/Info of several servers: services unioned by name, enabled when
/// any server is, and every proxy user may use it.
#[tokio::test]
async fn live_tv_info_and_guide_info_combine_servers() {
    let env = start("", |m, cfg| {
        if cfg.name == "s1" {
            m.on_json(
                None,
                "LiveTv/Info",
                json!({"Services": [{"Name": "Emby"}], "IsEnabled": true, "EnabledUsers": []}),
            );
            m.on_json(
                None,
                "LiveTv/GuideInfo",
                json!({"StartDate": "2026-11-01T00:00:00.0000000Z", "EndDate": "2026-11-05T00:00:00.0000000Z"}),
            );
        } else {
            m.on_json(
                None,
                "LiveTv/Info",
                json!({"Services": [{"Name": "Emby"}, {"Name": "HDHR"}], "IsEnabled": false, "EnabledUsers": []}),
            );
            m.on_json(
                None,
                "LiveTv/GuideInfo",
                json!({"StartDate": "2026-10-30T00:00:00.0000000Z", "EndDate": "2026-11-03T00:00:00.0000000Z"}),
            );
        }
    })
    .await;
    let me = env.get("Users/Me").await["Id"]
        .as_str()
        .unwrap()
        .to_string();
    let v = env.get("LiveTv/Info").await;
    let services: Vec<&str> = v["Services"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["Name"].as_str().unwrap())
        .collect();
    assert_eq!(services, ["Emby", "HDHR"], "{v}");
    assert_eq!(v["IsEnabled"], true);
    assert_eq!(v["EnabledUsers"], json!([me]));
    let g = env.get("LiveTv/GuideInfo").await;
    assert_eq!(g["StartDate"], "2026-10-30T00:00:00.0000000Z", "{g}");
    assert_eq!(g["EndDate"], "2026-11-05T00:00:00.0000000Z", "{g}");
}

/// Live TV management and tuner/provider administration are refused with an
/// empty 403 (proxy users are neither managers nor administrators), and
/// never reach an upstream.
#[tokio::test]
async fn live_tv_management_and_admin_routes_are_forbidden() {
    let env = start("", |_, _| {}).await;
    let x = id(&["x"]);
    let cases = [
        ("DELETE", format!("LiveTv/Recordings/{x}")),
        ("POST", "LiveTv/Timers".to_string()),
        ("POST", format!("LiveTv/Timers/{x}")),
        ("DELETE", format!("LiveTv/Timers/{x}")),
        ("POST", "LiveTv/SeriesTimers".to_string()),
        ("POST", format!("LiveTv/SeriesTimers/{x}")),
        ("DELETE", format!("LiveTv/SeriesTimers/{x}")),
        ("POST", format!("LiveTv/Tuners/{x}/Reset")),
        ("GET", "LiveTv/Tuners/Discover".to_string()),
        ("GET", "LiveTv/Tuners/Discvover".to_string()),
        ("POST", "LiveTv/TunerHosts".to_string()),
        ("DELETE", "LiveTv/TunerHosts".to_string()),
        ("POST", "LiveTv/ListingProviders".to_string()),
        ("DELETE", "LiveTv/ListingProviders".to_string()),
        (
            "GET",
            "LiveTv/ListingProviders/SchedulesDirect/Countries".to_string(),
        ),
        ("GET", "LiveTv/ChannelMappingOptions".to_string()),
        ("POST", "LiveTv/ChannelMappings".to_string()),
    ];
    for (m, p) in cases {
        let (st, body) = env.req(m, &p, None).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{m} {p}");
        assert_eq!(body, Value::Null, "{m} {p}: empty body");
    }
    for i in 0..2 {
        assert!(env.mock(i).calls().is_empty(), "s{} was not asked", i + 1);
    }
}
