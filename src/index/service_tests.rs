use super::*;
use crate::backend::mock::MockBackend;
use crate::index::crawl::Crawled;
use crate::index::entry::Entry;
use crate::index::groups::GroupLookup;
use crate::store::MemoryStore;
use async_trait::async_trait;
use std::collections::BTreeMap;

/// Every server holds the same movie (tmdb 1) plus one of its own.
struct FakeCrawler;

#[async_trait]
impl Crawler for FakeCrawler {
    async fn crawl(&self, up: &Arc<dyn MediaBackend>) -> Result<Crawled, String> {
        let movie = |id: String, name: &str, tmdb: &str| Entry {
            id,
            kind: "Movie".into(),
            name: name.into(),
            sort_name: name.to_lowercase(),
            libraries: vec![format!("{:032x}", 900 + up.id().0)],
            providers: BTreeMap::from([("tmdb".to_string(), tmdb.to_string())]),
            ..Default::default()
        };
        Ok(Crawled {
            entries: vec![
                movie(format!("{:032x}", 1), "Shared", "1"),
                movie(format!("{:032x}", 100 + up.id().0), up.name(), up.name()),
            ],
            ratings: HashMap::new(),
            libraries: 1,
        })
    }
}

fn config(dedupe: bool) -> Config {
    let text = format!(
        r#"
[proxy]
listen = "127.0.0.1:0"
reload_interval_secs = 0
[merge]
dedupe = {dedupe}
[[user]]
name = "me"
password = "pw"
[[server]]
name = "a"
server = "http://a:8096"
api_key = "k"
user = "u"
[[server]]
name = "b"
server = "http://b:8096"
api_key = "k"
user = "u"
"#
    );
    Config::parse(&text, &crate::config::PasswordCache::default()).unwrap()
}

type Setup = (
    Arc<ServerRegistry>,
    Vec<Arc<MockBackend>>,
    Vec<Arc<dyn MediaBackend>>,
    crate::ids::HashedIdTable,
);

fn setup() -> Setup {
    let reg = ServerRegistry::with_names(["a", "b"]);
    let mocks: Vec<Arc<MockBackend>> = ["a", "b"]
        .iter()
        .map(|n| {
            let id = reg.id_for(n);
            Arc::new(MockBackend::new(id, n, id.0 as i64))
        })
        .collect();
    let set = mocks
        .iter()
        .map(|m| m.clone() as Arc<dyn MediaBackend>)
        .collect();
    let ids = crate::ids::HashedIdTable::new(reg.clone());
    (reg, mocks, set, ids)
}

#[tokio::test]
async fn crawls_groups_and_survives_restart() {
    let store: Arc<dyn Store> = Arc::new(MemoryStore::new());
    let (reg, _mocks, set, ids) = setup();
    let cfg = config(true);
    let groups = Arc::new(CanonicalGroups::new());
    let svc = IndexService::with_crawler(
        store.clone(),
        reg.clone(),
        groups.clone(),
        Box::new(FakeCrawler),
    );
    assert!(svc.snapshot(&cfg, &set).is_none(), "cold until crawled");
    svc.crawl_all(&set).await;
    svc.rebuild(&cfg, &set, &ids);
    let snap = svc.snapshot(&cfg, &set).expect("warm");
    assert_eq!(snap.len(), 3, "shared movie grouped");
    let shared = snap
        .items
        .iter()
        .find(|i| i.copies.len() == 2)
        .unwrap()
        .id
        .clone();
    assert_eq!(groups.group(&shared).unwrap().members.len(), 2);

    // A new service on the same store is warm without crawling and keeps
    // the canonical id.
    let groups2 = Arc::new(CanonicalGroups::new());
    let svc2 = IndexService::with_crawler(store, reg, groups2.clone(), Box::new(FakeCrawler));
    svc2.rebuild(&cfg, &set, &ids);
    let snap2 = svc2.snapshot(&cfg, &set).expect("warm from store");
    assert!(snap2.item(&shared).is_some_and(|i| i.id == shared));
    assert_eq!(svc2.crawled_servers().len(), 2);
}

#[tokio::test]
async fn new_server_goes_cold_and_down_servers_are_skipped() {
    let store: Arc<dyn Store> = Arc::new(MemoryStore::new());
    let (reg, mocks, set, ids) = setup();
    let cfg = config(false);
    let svc = IndexService::with_crawler(
        store,
        reg.clone(),
        Arc::new(CanonicalGroups::new()),
        Box::new(FakeCrawler),
    );
    svc.crawl_all(&set[..1]).await;
    svc.rebuild(&cfg, &set[..1], &ids);
    assert!(svc.snapshot(&cfg, &set).is_none(), "b not crawled yet");
    mocks[1].set_down(true);
    assert!(
        svc.snapshot(&cfg, &set).is_some(),
        "b is down: serve a's index"
    );
    svc.crawl_all(&set).await;
    svc.rebuild(&cfg, &set, &ids);
    // b is down: it is skipped and not crawled.
    assert_eq!(svc.crawled_servers().len(), 1);
}

/// Every server unreachable since start: nothing was crawled, so no snapshot
/// may be published (an empty one would read as a built index and let
/// parental checks pass items they cannot rate).
#[tokio::test]
async fn nothing_crawled_publishes_no_snapshot() {
    let store: Arc<dyn Store> = Arc::new(MemoryStore::new());
    let (reg, mocks, set, ids) = setup();
    let cfg = config(true);
    let svc = IndexService::with_crawler(
        store,
        reg,
        Arc::new(CanonicalGroups::new()),
        Box::new(FakeCrawler),
    );
    for m in &mocks {
        m.set_down(true);
    }
    svc.crawl_all(&set).await;
    svc.rebuild(&cfg, &set, &ids);
    assert!(svc.latest().is_none());
}

/// A live server the index has not crawled yet (added by a reload, down at
/// start, or failing to crawl): its items cannot be rated, so a restricted
/// user may not open them until it is covered. Unknown ids on covered
/// servers still pass, and unrestricted users are never affected.
#[tokio::test]
async fn items_of_an_uncovered_server_are_blocked_for_restricted_users() {
    let store: Arc<dyn Store> = Arc::new(MemoryStore::new());
    let (reg, _mocks, set, ids) = setup();
    let ids = Arc::new(ids);
    let mut cfg = config(true);
    let cache = crate::config::PasswordCache::default();
    let users = Config::parse(
        &format!(
            "{}\n[[user]]\nname = \"kid\"\npassword = \"pw\"\nmax_parental_rating = 100\n",
            "[[server]]\nname = \"a\"\nserver = \"http://a:8096\"\napi_key = \"k\"\nuser = \"u\""
        ),
        &cache,
    )
    .unwrap();
    cfg.users = users.users;
    let svc = Arc::new(IndexService::with_crawler(
        store,
        reg.clone(),
        Arc::new(CanonicalGroups::new()),
        Box::new(FakeCrawler),
    ));
    let parental = crate::parental::service::ParentalService::new(&cfg, svc.clone(), ids.clone());
    svc.crawl_all(&set[..1]).await;
    svc.note_coverage(&set);
    svc.rebuild(&cfg, &set, ids.as_ref());
    let kid = crate::users::user_id_for("kid");
    let on = |s: usize, n: u64| {
        let up = crate::types::UpstreamItemId::parse(&format!("{n:032x}")).unwrap();
        ids.encode(set[s].id(), &up).id
    };
    assert!(parental.blocks(&kid, &on(1, 77)), "b is live and uncovered");
    assert!(
        !parental.blocks(&kid, &on(0, 77)),
        "a is covered: unknown id passes"
    );
    assert!(!parental.blocks(&crate::users::user_id_for("me"), &on(1, 77)));
    svc.crawl_all(&set).await;
    svc.note_coverage(&set);
    svc.rebuild(&cfg, &set, ids.as_ref());
    assert!(!parental.blocks(&kid, &on(1, 77)), "b crawled");
}
