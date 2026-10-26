//! The proxy running entirely on in-memory backends: `MockFactory` replaces
//! the Jellyfin backend and a custom endpoint is added to the registry,
//! exercising the trait seams without any network upstream.

use jmp::backend::mock::MockFactory;
use jmp::endpoints::{done, Registry, Route};
use jmp::proxy::response::ok_json;
use jmp::proxy::{Client, Req};
use jmp::{AppState, Shared, StateOptions};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;

const AUTH: &str = r#"MediaBrowser Client="Test", Device="Test", DeviceId="dev1", Version="1.0""#;

const CONFIG: &str = r#"
[proxy]
listen = "127.0.0.1:0"
name = "Mock Proxy"
reload_interval_secs = 0

[[user]]
name = "me"
password = "pw"

[[server]]
name = "gummybear"
server = "https://some-domain:8493"
user = "gummybear"
password = "meow"

[[server]]
name = "other"
server = "http://other:8096"
api_key = "k"
user = "someone"

[logging]
filter = "warn"
"#;

async fn hello(_st: &Shared, _r: Req, c: &Client) -> jmp::endpoints::HandlerResult {
    done(ok_json(json!({"hello": c.user_id})))
}

fn movie(id: &str, name: &str) -> Value {
    json!({"Id": id, "Name": name, "Type": "Movie", "SortName": name})
}

#[tokio::test]
async fn proxy_on_mock_backends() {
    let data = tempfile::tempdir().unwrap();
    let text = format!("{CONFIG}\n",).replacen(
        "reload_interval_secs = 0",
        &format!(
            "reload_interval_secs = 0\ndata_dir = {:?}",
            data.path().display().to_string()
        ),
        1,
    );
    let cache = jmp::config::PasswordCache::default();
    let cfg = jmp::config::Config::parse(&text, &cache).unwrap();

    let factory = Arc::new(MockFactory::new(|m, cfg| {
        let movies_id = jmp::ids::hash_id(&["view", &cfg.name]);
        m.on_json(
            None,
            "UserViews",
            json!({"Items": [{"Id": movies_id, "Name": "Movies", "CollectionType": "movies", "Type": "CollectionFolder"}], "TotalRecordCount": 1}),
        );
        let item = jmp::ids::hash_id(&["item", &cfg.name]);
        m.on_json(
            None,
            "Items",
            json!({"Items": [movie(&item, &format!("Film from {}", cfg.name))], "TotalRecordCount": 1}),
        );
    }));
    let mut endpoints = Registry::default_jellyfin();
    endpoints.add_first(
        Route::auth("hello", jmp::handler!(hello))
            .at("jmp/hello")
            .get(),
    );

    let state = AppState::with_options(
        cfg,
        StateOptions {
            backend_factory: Some(factory.clone()),
            endpoints: Some(endpoints),
            password_cache: Some(cache),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(state.backends().len(), 2);
    assert_eq!(state.backends()[0].kind(), "mock");
    let app = jmp::build_app(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let base = format!("http://{addr}");
    let http = reqwest::Client::new();

    // Custom endpoint requires a token.
    let r = http.get(format!("{base}/jmp/hello")).send().await.unwrap();
    assert_eq!(r.status(), 401);

    let auth: Value = http
        .post(format!("{base}/Users/AuthenticateByName"))
        .header("Authorization", AUTH)
        .json(&json!({"Username": "me", "Pw": "pw"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = auth["AccessToken"].as_str().unwrap().to_string();

    let hello: Value = http
        .get(format!("{base}/jmp/hello"))
        .header("X-Emby-Token", &token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hello["hello"], auth["User"]["Id"]);

    // Both mocks' "Movies" merge into one library.
    let views: Value = http
        .get(format!("{base}/UserViews"))
        .header("X-Emby-Token", &token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let items = views["Items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{views}");
    assert_eq!(items[0]["Name"], "Movies");

    // Fan-out list merges both servers, with proxy ids.
    let list: Value = http
        .get(format!(
            "{base}/Items?Recursive=true&IncludeItemTypes=Movie"
        ))
        .header("X-Emby-Token", &token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: Vec<&str> = list["Items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["Name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Film from gummybear", "Film from other"], "{list}");
    let raw = jmp::ids::hash_id(&["item", "gummybear"]);
    assert_ne!(list["Items"][0]["Id"], raw.as_str());

    // Each mock saw the calls; the views request was served from the catalog cache
    // the second time.
    let created = factory.created();
    assert_eq!(created.len(), 2);
    let before = created[0].calls().len();
    let _ = http
        .get(format!("{base}/UserViews"))
        .header("X-Emby-Token", &token)
        .send()
        .await
        .unwrap();
    let views_calls = created[0]
        .calls()
        .iter()
        .skip(before)
        .filter(|c| {
            c.path_and_query
                .to_ascii_lowercase()
                .starts_with("userviews")
        })
        .count();
    assert_eq!(views_calls, 0, "views should come from the catalog cache");

    // A backend that is down is skipped; the other still answers.
    created[1].set_down(true);
    let list: Value = http
        .get(format!(
            "{base}/Items?Recursive=true&IncludeItemTypes=Movie"
        ))
        .header("X-Emby-Token", &token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["Items"].as_array().unwrap().len(), 1, "{list}");
}
