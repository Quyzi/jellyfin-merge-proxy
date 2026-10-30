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
    // Existing example is left alone.
    std::fs::write(&example, "# mine").unwrap();
    let msg = first_run_help(&cfg).unwrap();
    assert!(msg.contains("An annotated example is at"), "{msg}");
    assert_eq!(std::fs::read_to_string(&example).unwrap(), "# mine");
    std::fs::write(&cfg, "").unwrap();
    assert!(first_run_help(&cfg).is_none());
}

#[test]
fn logging_peek_reads_only_logging_section() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("jmp.toml");
    std::fs::write(
        &cfg,
        "[proxy]\nunknown_key = 1\n\n[logging]\nfilter = \"debug\"\nformat = \"json\"\n",
    )
    .unwrap();
    let l = LoggingConfig::peek(&cfg);
    assert_eq!(l.filter, "debug");
    assert_eq!(l.format, LogFormat::Json);
    // Missing file or broken TOML: defaults, the real error comes from load().
    assert_eq!(
        LoggingConfig::peek(&dir.path().join("nope.toml")),
        LoggingConfig::default()
    );
    std::fs::write(&cfg, "[logging\n").unwrap();
    assert_eq!(LoggingConfig::peek(&cfg), LoggingConfig::default());
}

#[test]
fn load_keeps_friendly_message_for_old_layout() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("jmp.toml");
    std::fs::write(&cfg, "[server]\nlisten = \"0.0.0.0:8096\"\n").unwrap();
    let err = Config::load(&cfg, &PasswordCache::default()).unwrap_err();
    assert!(err.to_string().contains("is now `[proxy]`"), "{err}");
}
#[test]
fn first_run_detects_directory_instead_of_file() {
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = dir.path().join("jmp.toml");
    // Create a directory with the config name (simulates Docker bind mount issue)
    std::fs::create_dir(&cfg_dir).unwrap();

    let msg = first_run_help(&cfg_dir).unwrap();
    assert!(msg.contains("is a directory, not a file"), "{msg}");
    assert!(msg.contains("Docker bind-mounts"), "{msg}");
    assert!(msg.contains("HOST"), "{msg}");
    assert!(msg.contains("65532"), "{msg}");
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

/// The exact upstream shape from the requirements must parse as-is.
#[test]
fn parses_exact_server_snippet() {
    const SNIPPET: &str = r#"
[[server]]
name = "gummybear"
server = "https://some-domain:8493"
user = "gummybear"
password = "meow"
"#;
    // Raw: the snippet alone deserialises.
    let raw: RawConfig = toml::from_str(SNIPPET).unwrap();
    assert_eq!(raw.upstreams.len(), 1);
    // Full config: plus a proxy login.
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
    // `[[upstream]]` is still accepted as an alias for `[[server]]`.
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
        // (A substring check would flake: base64 salt/hash may contain "pw".)
        assert!(u
            .password
            .phc()
            .split('$')
            .all(|seg| seg != "pw" && seg != "pw2"));
    }
    assert!(cfg.users[0].password.verify("pw"));
    assert!(!cfg.users[0].password.verify("nope"));
    assert!(cfg.users[1].password.verify("pw2"));
    // Debug output never contains secrets
    let dbg = format!("{cfg:?}");
    assert!(!dbg.contains("pw2") && !dbg.contains("\"k\""));
    // Reload: unchanged users reuse the cached hash (no re-hash, same salt)
    assert_eq!(cache.len(), 2);
    let cfg2 = Config::parse(MINI, &cache).unwrap();
    assert_eq!(cfg.users[0].password, cfg2.users[0].password);
    assert_eq!(cache.len(), 2);
    // Changed password -> new hash
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

#[test]
fn database_section_defaults_and_modes() {
    const BASE: &str = "[[user]]\nname = \"me\"\npassword = \"pw\"\n[[server]]\nname = \"a\"\nurl = \"http://a:8096\"\napi_key = \"k\"\n";
    let cache = PasswordCache::default();
    let cfg = Config::parse(BASE, &cache).unwrap();
    assert_eq!(cfg.database, DatabaseConfig::default());
    assert_eq!(cfg.database.journal_mode, JournalMode::Wal);
    assert_eq!(cfg.database.synchronous, Synchronous::Normal);
    assert_eq!(cfg.database.busy_timeout_ms, 5000);
    assert_eq!(
        cfg.database.resolved_path(Path::new("/var/lib/jmp")),
        Path::new("/var/lib/jmp/jmp.db")
    );

    for mode in JournalMode::ALL {
        let text = format!("{BASE}[database]\njournal_mode = \"{mode}\"\nsynchronous = \"full\"\npath = \"/x/state.db\"\nbusy_timeout_ms = 10\n");
        let db = Config::parse(&text, &cache).unwrap().database;
        assert_eq!(db.journal_mode, mode);
        assert_eq!(db.synchronous, Synchronous::Full);
        assert_eq!(db.busy_timeout_ms, 10);
        assert_eq!(db.resolved_path(Path::new("/d")), Path::new("/x/state.db"));
        assert_eq!(
            JournalMode::parse(&mode.as_str().to_uppercase()),
            Some(mode)
        );
    }
    assert_eq!(JournalMode::parse("wal2"), None);
    assert!(Config::parse(
        &format!("{BASE}[database]\njournal_mode = \"wal2\"\n"),
        &cache
    )
    .is_err());
    assert!(Config::parse(
        &format!("{BASE}[database]\nsynchronous = \"sometimes\"\n"),
        &cache
    )
    .is_err());
    assert!(Config::parse(&format!("{BASE}[database]\nbogus = 1\n"), &cache).is_err());
}

#[test]
fn user_accounts_and_rating_limits() {
    let cache = PasswordCache::default();
    let text = r#"
[[user]]
name = "kid"
password = "pw"
max_parental_rating = "PG"
block_unrated = true
[[user.account]]
server = "X"
user = "kid"
password = "secret-kid"
[[user]]
name = "teen"
password = "pw"
max_parental_rating = 13
account = [{ server = "x", api_key = "k2", user_id = "abc" }]
[[server]]
name = "x"
server = "http://x:8096"
api_key = "k"
"#;
    let cfg = Config::parse(text, &cache).unwrap();
    let kid = &cfg.users[0];
    assert_eq!(kid.accounts[0].username.as_deref(), Some("kid"));
    assert_eq!(
        kid.max_parental_rating,
        Some(RatingLimit::Name("PG".into()))
    );
    assert!(kid.block_unrated);
    assert_eq!(
        cfg.users[1].max_parental_rating,
        Some(RatingLimit::Score(13))
    );
    assert!(!format!("{cfg:?}").contains("secret-kid"));
    let unknown = text.replace("server = \"X\"", "server = \"nope\"");
    assert!(Config::parse(&unknown, &cache).is_err());
    let no_pw = text.replace("password = \"secret-kid\"\n", "");
    assert!(Config::parse(&no_pw, &cache).is_err());
    let dup = text.replace(
        "account = [{ server = \"x\", api_key = \"k2\", user_id = \"abc\" }]",
        "account = [{ server = \"x\", api_key = \"k2\" }, { server = \"x\", api_key = \"k3\" }]",
    );
    assert!(Config::parse(&dup, &cache).is_err());
}
