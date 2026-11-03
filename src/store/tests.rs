use super::*;
use crate::config::{DatabaseConfig, JournalMode};

pub(crate) fn pid(n: u8) -> ProxyItemId {
    ProxyItemId::parse(&format!("{n:032x}")).unwrap()
}

pub(crate) fn session(token: &str, user_id: &str) -> Session {
    Session {
        token: token.into(),
        session_id: format!("s-{token}"),
        user_name: "u".into(),
        user_id: user_id.into(),
        client: "c".into(),
        device_name: "d".into(),
        device_id: "di".into(),
        app_version: "1".into(),
        created: "2026-01-01T00:00:00Z".into(),
    }
}

fn row(n: u8, server: &str) -> IdRow {
    IdRow {
        proxy_id: pid(n),
        server: server.into(),
        upstream_id: UpstreamItemId::parse(&format!("{:032x}", 1000 + n as u32)).unwrap(),
    }
}

/// Behaviour every [`Store`] must have; run against each implementation.
pub(crate) fn conformance(s: &dyn Store) {
    assert!(s.load_ids().unwrap().is_empty());
    s.append_ids(&[row(1, "a"), row(2, "B")]).unwrap();
    s.append_ids(&[row(2, "B"), row(3, "a")]).unwrap();
    assert_eq!(
        s.load_ids().unwrap(),
        vec![row(1, "a"), row(2, "B"), row(3, "a")],
        "insertion order, duplicates ignored, names verbatim"
    );

    s.append_links(&[(pid(1), pid(2)), (pid(1), pid(3))])
        .unwrap();
    s.append_links(&[(pid(1), pid(2))]).unwrap();
    assert_eq!(
        s.load_links().unwrap(),
        vec![(pid(1), pid(2)), (pid(1), pid(3))]
    );

    s.put_session(&session("t1", "u1")).unwrap();
    s.put_session(&session("t2", "u2")).unwrap();
    let mut changed = session("t1", "u1");
    changed.device_name = "other".into();
    s.put_session(&changed).unwrap();
    let got = s.load_sessions().unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got.iter().find(|x| x.token == "t1").unwrap(), &changed);
    s.remove_sessions(&["t1".into(), "missing".into()]).unwrap();
    assert_eq!(s.load_sessions().unwrap(), vec![session("t2", "u2")]);

    let batch = ImportBatch {
        ids: vec![row(3, "a"), row(4, "c")],
        links: vec![(pid(3), pid(4))],
        sessions: vec![session("t3", "u3")],
    };
    s.import(&batch).unwrap();
    assert_eq!(s.load_ids().unwrap().len(), 4);
    assert_eq!(s.load_links().unwrap().len(), 3);
    assert_eq!(s.load_sessions().unwrap().len(), 2);

    user_data(s);
    local(s);
}

fn user_data(s: &dyn Store) {
    use crate::userdata::ItemState;
    let ud = |user: &str, n: u8, played: bool| UserDataRow {
        user: user.into(),
        item: pid(n),
        state: ItemState {
            played,
            position_ticks: 7,
            ..Default::default()
        },
    };
    assert!(s.load_user_data().unwrap().is_empty());
    s.put_user_data(&[ud("a", 1, false), ud("b", 1, true)])
        .unwrap();
    s.put_user_data(&[ud("a", 1, true)]).unwrap();
    let mut got = s.load_user_data().unwrap();
    got.sort_by(|x, y| x.user.cmp(&y.user));
    assert_eq!(got, vec![ud("a", 1, true), ud("b", 1, true)], "upsert");
    s.mark_seeded("a", "srv").unwrap();
    s.mark_seeded("a", "srv").unwrap();
    assert_eq!(s.load_seeded().unwrap(), vec![("a".into(), "srv".into())]);
}

fn local(s: &dyn Store) {
    use serde_json::json;
    assert!(s.load_settings().unwrap().is_empty());
    s.put_settings("u", &json!({"a": 1})).unwrap();
    s.put_settings("u", &json!({"a": 2})).unwrap();
    assert_eq!(
        s.load_settings().unwrap(),
        vec![("u".into(), json!({"a": 2}))]
    );

    s.put_display_prefs("u", "p", "emby", &json!({"x": 1}))
        .unwrap();
    s.put_display_prefs("u", "p", "emby", &json!({"x": 2}))
        .unwrap();
    s.put_display_prefs("u", "p", "other", &json!({"x": 3}))
        .unwrap();
    let mut got = s.load_display_prefs().unwrap();
    got.sort_by(|a, b| a.2.cmp(&b.2));
    assert_eq!(
        got,
        vec![
            ("u".into(), "p".into(), "emby".into(), json!({"x": 2})),
            ("u".into(), "p".into(), "other".into(), json!({"x": 3})),
        ],
        "client is part of the key"
    );

    assert!(s.load_playlists().unwrap().is_empty());
    s.put_playlist("p1", "u", &json!({"id": "p1", "n": 1}))
        .unwrap();
    s.put_playlist("p1", "u", &json!({"id": "p1", "n": 2}))
        .unwrap();
    s.put_playlist("p2", "u", &json!({"id": "p2"})).unwrap();
    s.delete_playlist("p2").unwrap();
    assert_eq!(
        s.load_playlists().unwrap(),
        vec![json!({"id": "p1", "n": 2})]
    );
}

#[test]
fn memory_store_conformance() {
    conformance(&MemoryStore::new());
}

#[test]
fn sqlite_store_conformance() {
    let s = SqliteStore::open_in_memory(&DatabaseConfig::default()).unwrap();
    conformance(&s);
    let dir = tempfile::tempdir().unwrap();
    let s = SqliteStore::open(&dir.path().join("jmp.db"), &DatabaseConfig::default()).unwrap();
    conformance(&s);
}

#[test]
fn sqlite_store_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("jmp.db");
    let cfg = DatabaseConfig::default();
    {
        let s = SqliteStore::open(&path, &cfg).unwrap();
        s.append_ids(&[row(7, "Movies")]).unwrap();
        s.append_links(&[(pid(7), pid(8))]).unwrap();
        s.put_session(&session("tok", "u")).unwrap();
    }
    let s = SqliteStore::open(&path, &cfg).unwrap();
    assert_eq!(s.load_ids().unwrap(), vec![row(7, "Movies")]);
    assert_eq!(s.load_links().unwrap(), vec![(pid(7), pid(8))]);
    assert_eq!(s.load_sessions().unwrap(), vec![session("tok", "u")]);
    assert_eq!(s.schema_version(), migrations::latest());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn journal_mode_is_read_back() {
    let dir = tempfile::tempdir().unwrap();
    for mode in [JournalMode::Wal, JournalMode::Delete, JournalMode::Truncate] {
        let cfg = DatabaseConfig {
            journal_mode: mode,
            ..Default::default()
        };
        let path = dir.path().join(format!("{mode}.db"));
        let s = SqliteStore::open(&path, &cfg).unwrap();
        assert_eq!(s.journal_mode(), mode);
        let c = rusqlite::Connection::open(&path).unwrap();
        let on_disk: String = c
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        // WAL is a property of the file; the rollback modes are per connection.
        if mode == JournalMode::Wal {
            assert_eq!(on_disk, "wal");
        }
    }
}

#[test]
fn journal_mode_mismatch_reports_the_effective_mode() {
    // An in-memory database cannot use WAL: SQLite keeps "memory".
    let s = SqliteStore::open_in_memory(&DatabaseConfig::default()).unwrap();
    assert_eq!(s.journal_mode(), JournalMode::Memory);
    let c = rusqlite::Connection::open_in_memory().unwrap();
    assert_eq!(
        apply_journal_mode(&c, JournalMode::Delete).unwrap(),
        JournalMode::Memory
    );
    assert_eq!(
        apply_journal_mode(&c, JournalMode::Off).unwrap(),
        JournalMode::Off
    );
}

#[test]
fn open_creates_the_database_and_imports_legacy_files() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir(&data).unwrap();
    std::fs::write(
        data.join("ids.tsv"),
        "a\t0123456789abcdef0123456789abcdef\n",
    )
    .unwrap();
    let s = open(&DatabaseConfig::default(), &data).unwrap();
    assert_eq!(s.kind(), "sqlite");
    assert_eq!(s.load_ids().unwrap().len(), 1);
    assert!(data.join("jmp.db").exists());
    assert!(data.join("ids.tsv.migrated").exists());
    drop(s);

    // A custom path in a directory that does not exist yet.
    let cfg = DatabaseConfig {
        path: Some(dir.path().join("elsewhere/state.db")),
        journal_mode: JournalMode::Delete,
        ..Default::default()
    };
    let s = open(&cfg, &data).unwrap();
    assert!(s.load_ids().unwrap().is_empty());
    assert!(dir.path().join("elsewhere/state.db").exists());
}
