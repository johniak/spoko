use std::collections::BTreeMap;

use spoko_core::guard::{DeleteLog, GuardVerdict, MASS_DELETE_WINDOW_SECS, check_deletions};
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
    wm_of(0..n)
}

fn wm_of(files: impl IntoIterator<Item = usize>) -> WorkMap {
    let oid = git2::Oid::from_bytes(&[1; 20]).unwrap();
    let mut w = WorkMap::default();
    for i in files {
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

/// One capture going `prev` → `next` at `now`, recorded when the guard lets it through.
fn step(log: &mut DeleteLog, now: i64, prev: &WorkMap, next: &WorkMap) -> GuardVerdict {
    let (verdict, d) = log.check(now, prev, next);
    if verdict == GuardVerdict::Ok {
        log.commit(now, next, d);
    }
    verdict
}

#[test]
fn guard_judges_deletions_spread_over_captures_together() {
    let mut log = DeleteLog::default();
    assert_eq!(step(&mut log, 0, &wm(400), &wm_of(100..400)), GuardVerdict::Ok, "25 percent");
    assert_eq!(
        step(&mut log, 10, &wm_of(100..400), &wm_of(200..400)),
        GuardVerdict::MassDelete { deleted: 200, total: 400 }
    );
    // many small steps add up too
    let mut log = DeleteLog::default();
    let mut verdicts = vec![];
    for i in 0..10 {
        verdicts.push(step(&mut log, i, &wm_of(i as usize * 20..400), &wm_of((i as usize + 1) * 20..400)));
    }
    assert_eq!(verdicts.iter().position(|v| *v != GuardVerdict::Ok), Some(6), "{verdicts:?}");
    assert_eq!(verdicts[6], GuardVerdict::MassDelete { deleted: 140, total: 400 });
}

#[test]
fn guard_forgets_deletions_older_than_the_window() {
    let mut log = DeleteLog::default();
    assert_eq!(step(&mut log, 0, &wm(1000), &wm_of(200..1000)), GuardVerdict::Ok);
    let later = MASS_DELETE_WINDOW_SECS;
    assert_eq!(step(&mut log, later, &wm_of(200..1000), &wm_of(400..1000)), GuardVerdict::Ok, "200 of 800");
    assert_eq!(
        step(&mut log, later + 1, &wm_of(400..1000), &wm_of(500..1000)),
        GuardVerdict::MassDelete { deleted: 300, total: 800 }
    );
}

#[test]
fn guard_does_not_count_paths_that_came_back() {
    let mut log = DeleteLog::default();
    assert_eq!(step(&mut log, 0, &wm(100), &wm_of(40..100)), GuardVerdict::Ok);
    // the 40 are back (e.g. regenerated) and 40 others went
    assert_eq!(step(&mut log, 1, &wm_of(40..100), &wm_of((0..40).chain(80..100))), GuardVerdict::Ok);
    assert_eq!(
        step(&mut log, 2, &wm_of((0..40).chain(80..100)), &wm_of(0..30)),
        GuardVerdict::MassDelete { deleted: 70, total: 100 }
    );
    let mut cleared = log.clone();
    cleared.clear();
    assert_eq!(cleared, DeleteLog::default());
    assert_ne!(log, DeleteLog::default());
}
