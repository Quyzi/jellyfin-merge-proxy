//! Hot reload of the server set: start with one `[[server]]`, append a
//! second one to the config file, reload, see its libraries merged in;
//! change a URL (rebuild); remove the server and see it gone, with its old
//! ids answering 410 Gone and its backend drained and retired.

use jmp::backend::mock::{MockBackend, MockFactory};
use jmp::backend::MediaBackend;
use jmp::{AppState, Shared, StateOptions};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const AUTH: &str = r#"MediaBrowser Client="Test", Device="Test", DeviceId="dev1", Version="1.0""#;

fn base_config(data_dir: &Path, interval: u64) -> String {
    format!(
        r#"
[proxy]
listen = "127.0.0.1:0"
name = "Reload Proxy"
reload_interval_secs = {interval}
drain_timeout_secs = 5
data_dir = {data:?}

[[user]]
name = "me"
password = "pw"

[[server]]
name = "home"
server = "http://home:8096"
user = "u"
password = "p"

[logging]
filter = "warn"
"#,
        data = data_dir.display().to_string()
    )
}

const CABIN: &str = r#"
[[server]]
name = "cabin"
server = "http://cabin:8096"
user = "u"
password = "p"
"#;

fn movie(id: &str, name: &str) -> Value {
    json!({"Id": id, "Name": name, "Type": "Movie", "SortName": name})
}

/// "home" has Movies; "cabin" has Movies (merged with home's) and Shows.
fn factory() -> Arc<MockFactory> {
    Arc::new(MockFactory::new(|m: &MockBackend, cfg| {
        let mut views = vec![
            json!({"Id": jmp::ids::hash_id(&["movies", &cfg.name]), "Name": "Movies", "CollectionType": "movies", "Type": "CollectionFolder"}),
        ];
        if cfg.name == "cabin" {
            views.push(json!({"Id": jmp::ids::hash_id(&["shows", &cfg.name]), "Name": "Shows", "CollectionType": "tvshows", "Type": "CollectionFolder"}));
        }
        let n = views.len();
        m.on_json(
            None,
            "UserViews",
            json!({"Items": views, "TotalRecordCount": n}),
        );
        let item = jmp::ids::hash_id(&["item", &cfg.name]);
        m.on_json(
            None,
            "Items",
            json!({"Items": [movie(&item, &format!("Film from {}", cfg.name))], "TotalRecordCount": 1}),
        );
        m.on_json(
            None,
            &format!("Items/{item}/Images/Primary"),
            json!({"from": cfg.name, "host": cfg.url.trim_start_matches("http://")}),
        );
    }))
}

struct Harness {
    st: Shared,
    base: String,
    token: String,
    http: reqwest::Client,
    factory: Arc<MockFactory>,
}

async fn start(text: &str, path: &Path) -> Harness {
    std::fs::write(path, text).unwrap();
    let cache = jmp::config::PasswordCache::default();
    let cfg = jmp::config::Config::parse(text, &cache).unwrap();
    let factory = factory();
    let st = AppState::with_options(
        cfg,
        StateOptions {
            config_path: Some(path.to_path_buf()),
            backend_factory: Some(factory.clone()),
            password_cache: Some(cache),
            ..Default::default()
        },
    )
    .unwrap();
    let app = jmp::build_app(st.clone());
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
    Harness {
        st,
        base,
        token,
        http,
        factory,
    }
}

impl Harness {
    async fn get(&self, path: &str) -> (u16, Value) {
        let r = self
            .http
            .get(format!("{}/{path}", self.base))
            .header("X-Emby-Token", &self.token)
            .send()
            .await
            .unwrap();
        let s = r.status().as_u16();
        (s, r.json().await.unwrap_or(Value::Null))
    }
    async fn view_names(&self) -> Vec<String> {
        let (s, v) = self.get("UserViews").await;
        assert_eq!(s, 200, "{v}");
        let mut n: Vec<String> = v["Items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["Name"].as_str().unwrap().to_string())
            .collect();
        n.sort();
        n
    }
    /// (name, proxy id) of every movie in the merged list.
    async fn films(&self) -> Vec<(String, String)> {
        let (s, v) = self
            .get("Items?Recursive=true&IncludeItemTypes=Movie")
            .await;
        assert_eq!(s, 200, "{v}");
        v["Items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                (
                    i["Name"].as_str().unwrap().to_string(),
                    i["Id"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }
    fn mock(&self, name: &str, nth: usize) -> Arc<MockBackend> {
        self.factory
            .created()
            .into_iter()
            .filter(|m| m.name() == name)
            .nth(nth)
            .unwrap_or_else(|| panic!("no mock #{nth} for {name}"))
    }
}

async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..100 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for: {what}");
}

#[tokio::test]
async fn add_change_and_remove_a_server_at_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let one = base_config(&dir.path().join("data"), 0);
    let h = start(&one, &path).await;

    assert_eq!(h.st.backends().len(), 1);
    assert_eq!(h.view_names().await, ["Movies"]);
    assert_eq!(h.films().await.len(), 1);

    // --- add: append a [[server]] block to the file and reload ---
    let two = format!("{one}\n{CABIN}");
    std::fs::write(&path, &two).unwrap();
    assert!(h.st.reload(false).await.unwrap(), "reload applied");
    assert_eq!(h.st.backends().len(), 2);
    assert_eq!(h.st.backends()[1].name(), "cabin");
    // cabin's Movies merge into the existing Movies view; Shows is new.
    assert_eq!(h.view_names().await, ["Movies", "Shows"]);
    let films = h.films().await;
    let mut names: Vec<&str> = films.iter().map(|(n, _)| n.as_str()).collect();
    names.sort();
    assert_eq!(names, ["Film from cabin", "Film from home"], "{films:?}");
    let id_of = |n: &str| films.iter().find(|(f, _)| f == n).unwrap().1.clone();
    let cabin_film = id_of("Film from cabin");
    let home_film = id_of("Film from home");
    let (s, img) = h.get(&format!("Items/{cabin_film}/Images/Primary")).await;
    assert_eq!((s, img["from"].as_str()), (200, Some("cabin")), "{img}");

    // --- change home's URL: rebuilt in place, ids stay valid ---
    let moved = two.replacen("http://home:8096", "http://home.lan:8096", 1);
    std::fs::write(&path, &moved).unwrap();
    let old_home = h.mock("home", 0);
    let hold = old_home.hold(); // an in-flight stream on the old backend
    assert!(h.st.reload(false).await.unwrap());
    let new_home = h.mock("home", 1);
    assert_eq!(new_home.location().base, "http://home.lan:8096");
    let (s, img) = h.get(&format!("Items/{home_film}/Images/Primary")).await;
    assert_eq!(s, 200, "{img}");
    assert_eq!(
        img["host"], "home.lan:8096",
        "served by the rebuilt backend"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !old_home.is_retired(),
        "draining waits for in-flight streams"
    );
    drop(hold);
    eventually("old home backend retired", || old_home.is_retired()).await;
    assert!(!new_home.is_retired());

    // --- remove cabin: its libraries disappear, its ids answer 410 ---
    let cabin = h.mock("cabin", 0);
    std::fs::write(&path, moved.replace(CABIN, "")).unwrap();
    assert!(h.st.reload(false).await.unwrap());
    assert_eq!(h.st.backends().len(), 1);
    assert_eq!(h.view_names().await, ["Movies"]);
    let films = h.films().await;
    assert_eq!(films.len(), 1);
    assert_eq!(films[0], ("Film from home".to_string(), home_film));
    let (s, _) = h.get(&format!("Items/{cabin_film}/Images/Primary")).await;
    assert_eq!(s, 410, "ids of a removed server are Gone");
    eventually("cabin retired", || cabin.is_retired()).await;

    // --- re-add cabin: it gets its old ServerId back, old ids work again ---
    std::fs::write(&path, format!("{}\n{CABIN}", moved.replace(CABIN, ""))).unwrap();
    assert!(h.st.reload(false).await.unwrap());
    let (s, img) = h.get(&format!("Items/{cabin_film}/Images/Primary")).await;
    assert_eq!((s, img["from"].as_str()), (200, Some("cabin")), "{img}");

    // A broken new [[server]] rejects the whole reload; nothing changes.
    let bad = format!(
        "{}\n[[server]]\nname = \"bad\"\nserver = \"not a url\"\nuser = \"u\"\npassword = \"p\"\n",
        std::fs::read_to_string(&path).unwrap()
    );
    std::fs::write(&path, bad).unwrap();
    assert!(h.st.reload(false).await.is_err());
    assert_eq!(h.st.backends().len(), 2);
}

#[tokio::test]
async fn file_watcher_picks_up_a_new_server() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let one = base_config(&dir.path().join("data"), 1);
    let h = start(&one, &path).await;
    let _w = jmp::reload::spawn_watcher(Box::new(jmp::reload::PollingWatcher::new()), h.st.clone());
    tokio::time::sleep(Duration::from_millis(1200)).await;
    std::fs::write(&path, format!("{one}\n{CABIN}")).unwrap();
    let st = h.st.clone();
    eventually("watcher added cabin", move || st.backends().len() == 2).await;
    assert_eq!(h.view_names().await, ["Movies", "Shows"]);
    std::fs::write(&path, &one).unwrap();
    let st = h.st.clone();
    eventually("watcher removed cabin", move || st.backends().len() == 1).await;
    assert_eq!(h.view_names().await, ["Movies"]);
}
