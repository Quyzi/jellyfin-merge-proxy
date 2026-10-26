
use super::*;

fn reg(names: &[&str]) -> Arc<ServerRegistry> {
    let r = ServerRegistry::new();
    for n in names {
        r.id_for(n);
    }
    Arc::new(r)
}

#[test]
fn removed_servers_keep_their_ids() {
    let dir = tempfile::tempdir().unwrap();
    let up = "fedcba9876543210fedcba9876543210";
    let pid = {
        let m = HashedIdTable::new(reg(&["a", "gone"]))
            .with_persistence(dir.path())
            .unwrap();
        m.to_proxy(ServerId(1), up)
    };
    std::thread::sleep(std::time::Duration::from_millis(100));
    // "gone" is no longer configured: its ids still decode to its own id.
    let r = reg(&["a"]);
    let m2 = HashedIdTable::new(r.clone())
        .with_persistence(dir.path())
        .unwrap();
    let Some(Target::Item { upstream, .. }) = m2.resolve(&pid) else {
        panic!("id of removed server lost");
    };
    assert_eq!(r.name(upstream).as_deref(), Some("gone"));
    assert_eq!(r.lookup("a"), Some(ServerId(0)));
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
fn persistence() {
    let dir = tempfile::tempdir().unwrap();
    let up = "fedcba9876543210fedcba9876543210";
    let pid = {
        let m = HashedIdTable::new(reg(&["a"]))
            .with_persistence(dir.path())
            .unwrap();
        m.to_proxy(ServerId(0), up)
    };
    std::thread::sleep(std::time::Duration::from_millis(100));
    let m2 = HashedIdTable::new(reg(&["a"]))
        .with_persistence(dir.path())
        .unwrap();
    assert_eq!(m2.resolve_for(&pid, ServerId(0)).unwrap().as_str(), up);
}
