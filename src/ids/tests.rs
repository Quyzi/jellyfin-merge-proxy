use super::*;

fn reg(names: &[&str]) -> Arc<ServerRegistry> {
    let r = ServerRegistry::new();
    for n in names {
        r.id_for(n);
    }
    Arc::new(r)
}

#[test]
fn round_trip() {
    let m = HashedIdTable::new(reg(&["a", "b"]));
    let up = "0123456789abcdef0123456789ABCDEF";
    let pa = m.to_proxy(ServerId(0), up);
    let pb = m.to_proxy(ServerId(1), up);
    assert_ne!(pa, pb, "same upstream id on different servers must differ");
    assert_eq!(pa.len(), 32);
    assert!(looks_like_id(&pa));
    match m.resolve(&pa).unwrap() {
        Target::Item { upstream, id } => {
            assert_eq!(upstream, ServerId(0));
            assert_eq!(id.as_str(), up.to_ascii_lowercase());
        }
        _ => panic!(),
    }
    // deterministic, and only new once
    let uid = UpstreamItemId::parse(up).unwrap();
    assert!(!m.encode(ServerId(0), &uid).new);
    assert_eq!(m.to_proxy(ServerId(0), up), pa);
    // dashed form of proxy id resolves too
    let dashed = format!(
        "{}-{}-{}-{}-{}",
        &pa[0..8],
        &pa[8..12],
        &pa[12..16],
        &pa[16..20],
        &pa[20..]
    );
    assert!(m.resolve(&dashed).is_some());
    assert_eq!(
        m.resolve_for(&pb, ServerId(1)).unwrap().as_str(),
        up.to_ascii_lowercase()
    );
    assert!(m.resolve_for(&pb, ServerId(0)).is_none());
    // non-guid passthrough
    assert_eq!(m.to_proxy(ServerId(0), "abc"), "abc");
    assert_eq!(m.size_of(ServerId(0)), 1);
}

#[test]
fn metered_decorator_is_transparent() {
    let m = MeteredIdMapper::new(HashedIdTable::new(reg(&["a"])), reg(&["a"]));
    let up = "fedcba9876543210fedcba9876543210";
    let p = m.to_proxy(ServerId(0), up);
    assert_eq!(p, m.inner().to_proxy(ServerId(0), up));
    assert!(m.resolve("not-an-id").is_none());
    assert_eq!(m.resolve_for(&p, ServerId(0)).unwrap().as_str(), up);
}

#[test]
fn store_round_trip_keeps_ids() {
    use crate::config::DatabaseConfig;
    use crate::store::{IdStore, MemoryStore, SqliteStore};
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("jmp.db");
    let up = "fedcba9876543210fedcba9876543210";
    let stores: [Arc<dyn Fn() -> Arc<dyn IdStore>>; 2] = [
        {
            let mem: Arc<dyn IdStore> = Arc::new(MemoryStore::new());
            Arc::new(move || mem.clone())
        },
        Arc::new(move || {
            Arc::new(SqliteStore::open(&db, &DatabaseConfig::default()).unwrap())
                as Arc<dyn IdStore>
        }),
    ];
    for open in stores {
        let (pid, gone) = {
            let m = HashedIdTable::new(reg(&["a", "gone"]))
                .with_store(open())
                .unwrap();
            let pid = m.to_proxy(ServerId(0), up);
            assert_eq!(m.to_proxy(ServerId(0), up), pid, "saved once");
            (pid, m.to_proxy(ServerId(1), up))
            // Dropping the table flushes the writer: no sleep needed.
        };
        assert_eq!(open().load_ids().unwrap().len(), 2);
        let r = reg(&["a"]);
        let m2 = HashedIdTable::new(r.clone()).with_store(open()).unwrap();
        assert_eq!(m2.resolve_for(&pid, ServerId(0)).unwrap().as_str(), up);
        let Some(Target::Item { upstream, .. }) = m2.resolve(&gone) else {
            panic!("id of removed server lost");
        };
        assert_eq!(r.name(upstream).as_deref(), Some("gone"));
        assert_eq!(m2.size_of(ServerId(0)), 1);
    }
}

#[test]
fn grouped_ids_fail_over_and_resolve_per_server() {
    use crate::index::groups::{CanonicalGroups, GroupMap};
    let table = HashedIdTable::new(reg(&["a", "b"]));
    let up = "0123456789abcdef0123456789abcdef";
    let pa = ProxyItemId::parse(&table.to_proxy(ServerId(0), up)).unwrap();
    let pb = ProxyItemId::parse(&table.to_proxy(ServerId(1), up)).unwrap();
    let groups = Arc::new(CanonicalGroups::new());
    let mut map = GroupMap::new();
    map.insert(pa.clone(), vec![pa.clone(), pb.clone()]);
    groups.publish(map);
    let m = GroupedIdMapper::new(table, groups);
    let down = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let d = down.clone();
    m.set_liveness(Arc::new(move |s| {
        !(s == ServerId(0) && d.load(std::sync::atomic::Ordering::Relaxed))
    }));
    let server = |t: Option<Target>| match t {
        Some(Target::Item { upstream, .. }) => upstream,
        _ => panic!("not an item"),
    };
    assert_eq!(server(m.decode(&pa)), ServerId(0), "canonical = best copy");
    assert_eq!(
        server(m.decode(&pb)),
        ServerId(1),
        "member decodes literally"
    );
    down.store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        server(m.decode(&pa)),
        ServerId(1),
        "fails over to a live copy"
    );
    assert_eq!(
        m.resolve_for(pa.as_str(), ServerId(1)).unwrap().as_str(),
        up,
        "canonical id resolves to the copy on the asked server"
    );
    // Ranking changed (s2 preferred): the canonical id follows the best copy.
    down.store(false, std::sync::atomic::Ordering::Relaxed);
    let groups = Arc::new(CanonicalGroups::new());
    let mut map = GroupMap::new();
    map.insert(pa.clone(), vec![pb.clone(), pa.clone()]);
    groups.publish(map);
    let m = GroupedIdMapper::new(HashedIdTable::new(reg(&["a", "b"])), groups);
    m.encode(ServerId(0), &UpstreamItemId::parse(up).unwrap());
    m.encode(ServerId(1), &UpstreamItemId::parse(up).unwrap());
    assert_eq!(server(m.decode(&pa)), ServerId(1));
}
