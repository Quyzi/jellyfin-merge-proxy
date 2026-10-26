
use super::*;
use crate::password::PasswordCache;
use crate::tls::RustlsProvider;

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
