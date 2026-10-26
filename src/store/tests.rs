use super::*;

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
}

#[test]
fn memory_store_conformance() {
    conformance(&MemoryStore::new());
}
