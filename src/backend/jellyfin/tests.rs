use super::*;
use crate::password::PasswordCache;
use crate::tls::RustlsProvider;
use std::collections::HashMap;

fn cfg(priority: &str) -> Config {
    let text = format!(
        "[[user]]\nname = \"u\"\npassword = \"p\"\n\
             [[server]]\nname = \"a\"\nserver = \"http://127.0.0.1:1\"\n\
             user = \"x\"\npassword = \"y\"\n{priority}\n"
    );
    Config::parse(&text, &PasswordCache::default()).unwrap()
}

/// Found against real servers: a priority-only reload kept the old
/// dedupe winner because the priority was fixed at construction.
#[tokio::test]
async fn priority_reloads_in_place() {
    let old = cfg("priority = 5");
    let b =
        JellyfinBackend::new(ServerId(0), old.upstreams[0].clone(), &old, &RustlsProvider).unwrap();
    assert_eq!(b.priority(), 5);
    let new = cfg("priority = -1");
    let cx = ReloadContext {
        old: &old,
        new: &new,
        files_changed: false,
        tls: &RustlsProvider,
    };
    assert!(b.apply(&cx).await.unwrap());
    assert_eq!(b.priority(), -1);
    // Removing the key falls back to the position default.
    let unset = cfg("");
    let cx = ReloadContext {
        old: &new,
        new: &unset,
        files_changed: false,
        tls: &RustlsProvider,
    };
    b.apply(&cx).await.unwrap();
    assert_eq!(b.priority(), 0);
}

#[test]
fn versions() {
    assert!(super::version_at_least("10.9.0", 10, 9));
    assert!(super::version_at_least("10.10.7", 10, 9));
    assert!(!super::version_at_least("10.8.13", 10, 9));
}

/// Answers login calls and records which token each other call carried.
#[derive(Default)]
struct FakeJellyfin {
    seen: std::sync::Mutex<Vec<(String, String)>>,
    /// Tokens that get one 401 (then are "revoked").
    expire: std::sync::Mutex<Vec<String>>,
    logins: std::sync::atomic::AtomicUsize,
    /// Login user -> the DeviceId it logged in with.
    devices: std::sync::Mutex<HashMap<String, String>>,
}

impl Transport for FakeJellyfin {
    fn send(
        &self,
        req: reqwest::Request,
        _streaming: bool,
    ) -> futures_util::future::BoxFuture<'_, Result<reqwest::Response, reqwest::Error>> {
        let path = req.url().path().to_string();
        let token = req
            .headers()
            .get("X-Emby-Token")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = req
            .body()
            .and_then(|b| b.as_bytes())
            .map(|b| serde_json::from_slice::<Value>(b).unwrap_or(Value::Null));
        let (status, v) = if path.ends_with("System/Info/Public") {
            (200, serde_json::json!({"Version": "10.11.0"}))
        } else if path.ends_with("Users/AuthenticateByName") {
            let n = self
                .logins
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let user = body.unwrap()["Username"].as_str().unwrap().to_string();
            let auth = req.headers()["Authorization"].to_str().unwrap();
            let dev = auth.split("DeviceId=\"").nth(1).unwrap();
            let dev = dev.split('"').next().unwrap().to_string();
            self.devices.lock().unwrap().insert(user.clone(), dev);
            (
                200,
                serde_json::json!({"AccessToken": format!("tok-{user}-{n}"), "User": {"Id": format!("id-{user}")}}),
            )
        } else {
            self.seen.lock().unwrap().push((path, token.clone()));
            let mut ex = self.expire.lock().unwrap();
            if let Some(i) = ex.iter().position(|t| token.starts_with(t.as_str())) {
                ex.remove(i);
                (401, Value::Null)
            } else {
                (200, serde_json::json!({"ok": true}))
            }
        };
        let resp = axum::http::Response::builder()
            .status(status)
            .body(serde_json::to_vec(&v).unwrap())
            .unwrap();
        Box::pin(async move { Ok(reqwest::Response::from(resp)) })
    }
}

fn accounts_cfg(kid_pw: &str) -> Config {
    let text = format!(
        "[[user]]\nname = \"kid\"\npassword = \"p\"\n\
         [[user.account]]\nserver = \"a\"\nuser = \"kid\"\npassword = \"{kid_pw}\"\n\
         [[user]]\nname = \"adult\"\npassword = \"p\"\n\
         [[server]]\nname = \"a\"\nserver = \"http://127.0.0.1:1\"\n\
         user = \"shared\"\npassword = \"y\"\n"
    );
    Config::parse(&text, &PasswordCache::default()).unwrap()
}

#[tokio::test]
async fn per_user_accounts_carry_their_own_token() {
    let cfg = accounts_cfg("kpw");
    let b =
        JellyfinBackend::new(ServerId(0), cfg.upstreams[0].clone(), &cfg, &RustlsProvider).unwrap();
    let fake = Arc::new(FakeJellyfin::default());
    b.set_transport(fake.clone());
    let kid = crate::users::user_id_for("kid");
    let adult = crate::users::user_id_for("adult");
    assert!(b.has_account(&kid) && !b.has_account(&adult));
    assert_eq!(b.status().await.user_accounts, 1);
    // Sessions name each account's upstream user.
    let s = b.login_as(Some(&kid)).await.unwrap();
    assert_eq!(s.user_id.as_deref(), Some("id-kid"));
    let s = b.login_as(Some(&adult)).await.unwrap();
    assert_eq!(s.user_id.as_deref(), Some("id-shared"));
    b.get_json_as(Some(&kid), "Items").await.unwrap();
    b.get_json_as(Some(&adult), "Items").await.unwrap();
    b.get_json("Items").await.unwrap();
    {
        let seen = fake.seen.lock().unwrap();
        let toks: Vec<&str> = seen.iter().map(|(_, t)| t.as_str()).collect();
        assert!(toks[0].starts_with("tok-kid-"), "{toks:?}");
        assert!(toks[1].starts_with("tok-shared-") && toks[2] == toks[1]);
    }
    // A 401 on the kid's token re-logs in only the kid's account.
    fake.expire.lock().unwrap().push("tok-kid-".into());
    let before = fake.logins.load(std::sync::atomic::Ordering::Relaxed);
    let (st, _) = b.get_json_as(Some(&kid), "Items").await.unwrap();
    assert!(st.is_success());
    assert_eq!(
        fake.logins.load(std::sync::atomic::Ordering::Relaxed),
        before + 1
    );
    // Changing the account's password on reload drops its login.
    let new = accounts_cfg("kpw2");
    let cx = ReloadContext {
        old: &cfg,
        new: &new,
        files_changed: false,
        tls: &RustlsProvider,
    };
    assert!(b.apply(&cx).await.unwrap());
    let before = fake.logins.load(std::sync::atomic::Ordering::Relaxed);
    b.login_as(Some(&kid)).await.unwrap();
    assert_eq!(
        fake.logins.load(std::sync::atomic::Ordering::Relaxed),
        before + 1
    );
    assert!(!format!("{:?}", b.accounts).contains("kpw"));
    // Jellyfin ends older sessions of a device: accounts use their own.
    let devs = fake.devices.lock().unwrap();
    assert_ne!(devs["kid"], devs["shared"]);
}
