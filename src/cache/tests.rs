
use super::*;
use serde_json::json;

fn settings(ttl_ms: u64, max: usize) -> CacheSettings {
    CacheSettings {
        ttl: Duration::from_millis(ttl_ms),
        max_entries: max,
        serve_stale_on_error: true,
    }
}

#[test]
fn ttl_and_eviction() {
    let c = MeteredCache(MemoryCatalogCache::new(settings(50, 2)));
    let k = |s: &str| CacheKey::new("views", s);
    assert_eq!(c.get(&k("a")), Lookup::Miss);
    c.put(k("a"), json!([1]));
    assert_eq!(c.get(&k("a")), Lookup::Fresh(json!([1])));
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(c.get(&k("a")), Lookup::Stale(json!([1])));
    c.put(k("b"), json!([2]));
    let o = c.put(k("c"), json!([3]));
    assert_eq!(o.evicted, 1, "max_entries = 2 evicts the oldest");
    assert_eq!(c.get(&k("a")), Lookup::Miss);
    assert_eq!(c.invalidate_server("B"), 1, "case-insensitive, one entry");
    assert_eq!(c.get(&k("b")), Lookup::Miss);
    assert!(matches!(c.get(&k("c")), Lookup::Fresh(_)));
    c.invalidate_all();
    assert!(c.is_empty());
}

#[test]
fn disk_cache_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("catalog.json");
    {
        let c = DiskCatalogCache::open(p.clone(), settings(60_000, 10));
        c.put(CacheKey::new("views", "a"), json!([{"Name": "Movies"}]));
    }
    let c = DiskCatalogCache::open(p, settings(60_000, 10));
    assert_eq!(
        c.get(&CacheKey::new("views", "a")),
        Lookup::Fresh(json!([{"Name": "Movies"}]))
    );
}

#[test]
fn disabled_cache_never_hits() {
    let c = build(&CacheConfig {
        enabled: false,
        ..CacheConfig::default()
    });
    c.put(CacheKey::new("views", "a"), json!([]));
    assert_eq!(c.get(&CacheKey::new("views", "a")), Lookup::Miss);
    assert_eq!(c.name(), "none");
}
