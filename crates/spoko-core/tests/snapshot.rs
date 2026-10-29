use std::collections::BTreeMap;

use spoko_core::guard::{GuardVerdict, check_deletions};
use spoko_core::merge::Conflict;
use spoko_core::platform::RelPath;
use spoko_core::snapshot::{SnapshotMeta, SnapshotParts, read_snapshot, same_content, write_snapshot};
use spoko_core::store::{Entry, EntryMode, Store, WorkMap};
use uuid::Uuid;

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::open_or_init(&d.path().join("s.git")).unwrap();
    (d, s)
}

fn tree(s: &Store, files: &[(&str, &[u8])]) -> git2::Oid {
    let mut w = WorkMap::default();
    for (p, c) in files {
        w.insert(RelPath::new(p), Entry { oid: s.write_blob(c).unwrap(), mode: EntryMode::Blob });
    }
    s.build_tree(&w).unwrap()
}

fn meta(time: i64) -> SnapshotMeta {
    SnapshotMeta { device_id: Uuid::nil(), device_name: "mac".into(), time, conflicts: vec![] }
}

#[test]
fn snapshot_roundtrip() {
    let (_d, s) = store();
    let mut m = meta(10);
    m.conflicts.push(Conflict { path: "work/a".into(), base: None, theirs: None, theirs_device: "x".into(), at: 1 });
    let parts = SnapshotParts {
        work: tree(&s, &[("a", b"a")]),
        gitstate: Some(tree(&s, &[("state.json", b"{}")])),
        externals: BTreeMap::from([("claude".to_string(), tree(&s, &[("s.jsonl", b"{}")]))]),
        meta: m,
    };
    let c = write_snapshot(&s, &parts, &[]).unwrap();
    assert_eq!(read_snapshot(&s, c).unwrap(), parts);
    let c2 = write_snapshot(&s, &parts, &[c]).unwrap();
    assert_eq!(s.repo().find_commit(c2).unwrap().parent_ids().collect::<Vec<_>>(), vec![c]);
    let root = s.commit_tree(c).unwrap();
    assert!(s.subtree(root, "meta").unwrap().is_some());
}

#[test]
fn snapshot_without_gitstate_or_externals() {
    let (_d, s) = store();
    let parts = SnapshotParts { work: tree(&s, &[]), gitstate: None, externals: BTreeMap::new(), meta: meta(1) };
    let c = write_snapshot(&s, &parts, &[]).unwrap();
    assert_eq!(read_snapshot(&s, c).unwrap(), parts);
}

#[test]
fn same_content_ignores_time_and_device() {
    let (_d, s) = store();
    let work = tree(&s, &[("a", b"a")]);
    let p1 = SnapshotParts { work, gitstate: None, externals: BTreeMap::new(), meta: meta(1) };
    let mut p2 = p1.clone();
    p2.meta.time = 99;
    p2.meta.device_name = "other".into();
    let (c1, c2) = (write_snapshot(&s, &p1, &[]).unwrap(), write_snapshot(&s, &p2, &[]).unwrap());
    assert!(same_content(&s, c1, c2).unwrap());
    let mut p3 = p1.clone();
    p3.work = tree(&s, &[("a", b"b")]);
    assert!(!same_content(&s, c1, write_snapshot(&s, &p3, &[]).unwrap()).unwrap());
    let mut p4 = p1.clone();
    p4.meta.conflicts.push(Conflict { path: "work/a".into(), base: None, theirs: None, theirs_device: "x".into(), at: 1 });
    assert!(!same_content(&s, c1, write_snapshot(&s, &p4, &[]).unwrap()).unwrap());
}

fn wm(n: usize) -> WorkMap {
    let oid = git2::Oid::from_bytes(&[1; 20]).unwrap();
    let mut w = WorkMap::default();
    for i in 0..n {
        w.insert(RelPath::new(&format!("f{i}")), Entry { oid, mode: EntryMode::Blob });
    }
    w
}

#[test]
fn guard_thresholds() {
    assert_eq!(check_deletions(&wm(100), &wm(51)), GuardVerdict::Ok, "49 deleted ≤ 50");
    assert_eq!(check_deletions(&wm(100), &wm(40)), GuardVerdict::MassDelete { deleted: 60, total: 100 });
    assert_eq!(check_deletions(&wm(1000), &wm(940)), GuardVerdict::Ok, "6 percent");
    assert_eq!(check_deletions(&WorkMap::default(), &WorkMap::default()), GuardVerdict::Ok);
}
