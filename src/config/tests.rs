use super::*;

#[test]
fn first_run_writes_example_once_and_never_the_config() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("sub/jmp.toml");
    let msg = first_run_help(&cfg).unwrap();
    assert!(msg.contains("Wrote an annotated example"), "{msg}");
    let example = dir.path().join("sub/jmp.example.toml");
    assert_eq!(std::fs::read_to_string(&example).unwrap(), EXAMPLE);
    assert!(!cfg.exists());
    std::fs::write(&example, "# mine").unwrap();
    let msg = first_run_help(&cfg).unwrap();
    assert!(msg.contains("An annotated example is at"), "{msg}");
    assert_eq!(std::fs::read_to_string(&example).unwrap(), "# mine");
    std::fs::write(&cfg, "").unwrap();
    assert!(first_run_help(&cfg).is_none());
}

#[test]
fn parses_example() {
    let cache = PasswordCache::default();
    let cfg = Config::parse(include_str!("../../config.example.toml"), &cache).unwrap();
    assert_eq!(cfg.upstreams.len(), 2);
    assert_eq!(cfg.upstreams[0].name, "gummybear");
    assert_eq!(cfg.upstreams[0].url, "https://some-domain:8493");
    assert!(!cfg.users.is_empty());
    assert!(cfg.tls.is_none());
    assert!(cfg.cache.enabled);
}

#[test]
fn parses_exact_server_snippet() {
    const SNIPPET: &str = r#"
[[server]]
name = "gummybear"
server = "https://some-domain:8493"
user = "gummybear"
password = "meow"
"#;
    let raw: RawConfig = toml::from_str(SNIPPET).unwrap();
    assert_eq!(raw.upstreams.len(), 1);
    let cache = PasswordCache::default();
    let text = format!("{SNIPPET}\n[[user]]\nname = \"me\"\npassword = \"pw\"\n");
    let cfg = Config::parse(&text, &cache).unwrap();
    let s = &cfg.upstreams[0];
    assert_eq!(s.name, "gummybear");
    assert_eq!(s.url, "https://some-domain:8493");
    assert_eq!(s.username.as_deref(), Some("gummybear"));
    assert_eq!(s.password.as_ref().unwrap().expose(), "meow");
    assert!(s.api_key.is_none() && s.tls.is_none() && s.enabled);
    assert_eq!(cfg.proxy, ProxyConfig::default());
    assert_eq!(cfg.cache, CacheConfig::default());
    assert!(!format!("{cfg:?}").contains("meow"));
}

#[test]
fn server_tls_subtable_aliases_and_old_layout_hint() {
    let cache = PasswordCache::default();
    let text = r#"
[proxy]
listen = "127.0.0.1:1"
[[user]]
name = "me"
password = "pw"
[[server]]
name = "a"
server = "https://a:8920"
api_key = "k"
[server.tls]
insecure_skip_verify = true
[[server]]
name = "b"
url = "http://b:8096"
username = "u"
password = "p"
[cache]
ttl_secs = 5
max_entries = 10
persist_path = "/tmp/x.json"
"#;
    let cfg = Config::parse(text, &cache).unwrap();
    assert_eq!(cfg.upstreams.len(), 2);
    assert_eq!(
        cfg.upstreams[0].tls.as_ref().unwrap().insecure_skip_verify,
        Some(true)
    );
    assert_eq!(cfg.upstreams[1].username.as_deref(), Some("u"));
    assert_eq!(cfg.cache.ttl_secs, 5);
    assert_eq!(cfg.proxy.listen, "127.0.0.1:1");
    let alias = "[[user]]\nname = \"me\"\npassword = \"pw\"\n[[upstream]]\nname = \"b\"\nserver = \"http://b:8096\"\nuser = \"u\"\npassword = \"p\"\n";
    let cfg = Config::parse(alias, &cache).unwrap();
    assert_eq!(cfg.upstreams[0].name, "b");
    let old = "[server]\nlisten = \"x\"\n";
    let e = Config::parse(old, &cache).unwrap_err().to_string();
    assert!(e.contains("[proxy]"), "{e}");
}

const MINI: &str = r#"
[[user]]
name = "a"
password = "pw"
[[user]]
name = "b"
password = { plain = "pw2" }
[[server]]
name = "x"
server = "http://x:8096"
api_key = "k"
"#;

#[test]
fn plaintext_passwords_are_hashed_and_cached() {
    let cache = PasswordCache::default();
    let cfg = Config::parse(MINI, &cache).unwrap();
    for u in &cfg.users {
        assert!(
            u.password.phc().starts_with("$argon2id$"),
            "stored as argon2"
        );
        assert!(u
            .password
            .phc()
            .split('$')
            .all(|seg| seg != "pw" && seg != "pw2"));
    }
    assert!(cfg.users[0].password.verify("pw"));
    assert!(!cfg.users[0].password.verify("nope"));
    assert!(cfg.users[1].password.verify("pw2"));
    let dbg = format!("{cfg:?}");
    assert!(!dbg.contains("pw2") && !dbg.contains("\"k\""));
    assert_eq!(cache.len(), 2);
    let cfg2 = Config::parse(MINI, &cache).unwrap();
    assert_eq!(cfg.users[0].password, cfg2.users[0].password);
    assert_eq!(cache.len(), 2);
    let cfg3 = Config::parse(&MINI.replace("\"pw\"", "\"pw-new\""), &cache).unwrap();
    assert_ne!(cfg.users[0].password, cfg3.users[0].password);
    assert!(cfg3.users[0].password.verify("pw-new"));
}

#[test]
fn argon2_passthrough_and_validation() {
    let h = HashedPassword::hash("secret").unwrap();
    let cache = PasswordCache::default();
    let toml_bare = MINI.replace("\"pw\"", &format!("'{}'", h.phc()));
    let cfg = Config::parse(&toml_bare, &cache).unwrap();
    assert_eq!(
        cfg.users[0].password, h,
        "existing hash passes through unchanged"
    );
    let toml_tagged = MINI.replace("\"pw\"", &format!("{{ argon2 = '{}' }}", h.phc()));
    let cfg = Config::parse(&toml_tagged, &cache).unwrap();
    assert!(cfg.users[0].password.verify("secret"));
    let bad = MINI.replace("\"pw\"", "{ argon2 = 'not-a-hash' }");
    assert!(Config::parse(&bad, &cache).is_err());
}
