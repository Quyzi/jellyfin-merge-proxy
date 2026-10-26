use super::*;
use crate::config::DatabaseConfig;
use crate::ids::{HashedIdTable, IdMapper};
use crate::servers::ServerRegistry;
use crate::store::tests::{pid, session};
use crate::types::ServerId;
use std::sync::Arc;

const IDS_TSV: &str = "Movies\t0123456789ABCDEF0123456789ABCDEF\n\
second\tfedcba98-7654-3210-fedc-ba9876543210\n\
garbage line\n\
movies\tnot-an-id\n";

fn write_legacy(dir: &Path) {
    std::fs::write(dir.join(IDS_FILE), IDS_TSV).unwrap();
    let links = format!("{}\t{}\nbad\n", pid(1).as_str(), pid(2).as_str());
    std::fs::write(dir.join(SIBLINGS_FILE), links).unwrap();
    let sessions = vec![session("t1", "u1"), session("t2", "u2")];
    std::fs::write(
        dir.join(SESSIONS_FILE),
        serde_json::to_string_pretty(&sessions).unwrap(),
    )
    .unwrap();
}

#[test]
fn proxy_ids_are_preserved_exactly() {
    let rows = parse_ids(IDS_TSV);
    let got: Vec<_> = rows.iter().map(|r| r.proxy_id.as_str()).collect();
    // Golden values: sha256("jmp1:<lowercase name>:<id>")[..16], i.e. what
    // every earlier build handed to clients for these lines.
    assert_eq!(
        got,
        [
            "90ca555751686170f78b35469168e342",
            "d5bb059702a80d8d66caeac8626e9a77"
        ]
    );
    assert_eq!(rows[0].server, "Movies");
    assert_eq!(
        rows[0].upstream_id.as_str(),
        "0123456789abcdef0123456789abcdef"
    );
    // ...and what the id table computes at runtime.
    let reg = Arc::new(ServerRegistry::new());
    reg.id_for("movies");
    reg.id_for("Second");
    let table = HashedIdTable::new(reg);
    assert_eq!(
        table.to_proxy(ServerId(0), "0123456789ABCDEF0123456789ABCDEF"),
        got[0]
    );
    assert_eq!(
        table.to_proxy(ServerId(1), "fedcba9876543210fedcba9876543210"),
        got[1]
    );
}

#[test]
fn imports_once_and_renames() {
    let dir = tempfile::tempdir().unwrap();
    write_legacy(dir.path());
    let store = SqliteStore::open(&dir.path().join("jmp.db"), &DatabaseConfig::default()).unwrap();
    let r = import_legacy(&store, dir.path()).unwrap().unwrap();
    assert_eq!((r.ids, r.links, r.sessions), (2, 1, 2));
    assert_eq!(r.files.len(), 3);
    for f in [IDS_FILE, SIBLINGS_FILE, SESSIONS_FILE] {
        assert!(!dir.path().join(f).exists(), "{f} still there");
        assert!(
            dir.path().join(format!("{f}.migrated")).exists(),
            "{f}.migrated"
        );
    }
    assert_eq!(store.load_ids().unwrap(), parse_ids(IDS_TSV));
    assert_eq!(store.load_links().unwrap(), vec![(pid(1), pid(2))]);
    assert_eq!(
        store.load_sessions().unwrap(),
        vec![session("t1", "u1"), session("t2", "u2")]
    );
    // Nothing left to import on the next start.
    assert_eq!(import_legacy(&store, dir.path()).unwrap(), None);
    assert_eq!(store.load_ids().unwrap().len(), 2);

    // A later stray file is imported too, without clobbering the backup.
    std::fs::write(
        dir.path().join(IDS_FILE),
        "third\t11111111111111111111111111111111\n",
    )
    .unwrap();
    let r = import_legacy(&store, dir.path()).unwrap().unwrap();
    assert_eq!(r.ids, 1);
    assert!(dir.path().join("ids.tsv.migrated.1").exists());
    assert_eq!(store.load_ids().unwrap().len(), 3);
}

#[test]
fn unreadable_sessions_file_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(SESSIONS_FILE), "{not json").unwrap();
    let store = MemoryStore::new();
    assert_eq!(import_legacy(&store, dir.path()).unwrap(), None);
    assert!(dir.path().join(SESSIONS_FILE).exists());
}

#[test]
fn nothing_to_import() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        import_legacy(&MemoryStore::new(), dir.path()).unwrap(),
        None
    );
}
