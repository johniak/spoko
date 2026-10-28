use std::io::Write;

use spoko_core::platform::RelPath;
use spoko_core::store::{Entry, EntryMode, Store, WorkMap};

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::open_or_init(&d.path().join("store.git")).unwrap();
    (d, s)
}

#[test]
fn build_read_tree_roundtrip() {
    let (_d, s) = store();
    let a = s.write_blob(b"a").unwrap();
    let b = s.write_blob(b"#!/bin/sh").unwrap();
    let l = s.write_blob(b"target").unwrap();
    let commit = s.commit(s.build_tree(&WorkMap::default()).unwrap(), &[], "x", 1).unwrap();
    let mut w = WorkMap::default();
    w.insert(RelPath::new("src/lib/a.rs"), Entry { oid: a, mode: EntryMode::Blob });
    w.insert(RelPath::new("run.sh"), Entry { oid: b, mode: EntryMode::BlobExec });
    w.insert(RelPath::new("link"), Entry { oid: l, mode: EntryMode::Link });
    w.insert(RelPath::new("sub/repo"), Entry { oid: commit, mode: EntryMode::Commit });
    w.insert(RelPath::new("src/z.rs"), Entry { oid: a, mode: EntryMode::Blob });
    let tree = s.build_tree(&w).unwrap();
    assert_eq!(s.read_tree(tree).unwrap(), w);
    assert_eq!(s.build_tree(&w).unwrap(), tree);
    let src = s.subtree(tree, "src").unwrap().unwrap();
    assert_eq!(s.read_tree(src).unwrap().len(), 2);
    assert_eq!(s.subtree(tree, "nope").unwrap(), None);
    assert_eq!(s.subtree(tree, "run.sh").unwrap(), None);
}

#[test]
fn prebuilt_subtree_entries_are_inserted_as_is() {
    let (_d, s) = store();
    let mut inner = WorkMap::default();
    inner.insert(RelPath::new("x"), Entry { oid: s.write_blob(b"x").unwrap(), mode: EntryMode::Blob });
    let inner_tree = s.build_tree(&inner).unwrap();
    let mut outer = WorkMap::default();
    outer.insert(RelPath::new("gitstate/index"), Entry { oid: inner_tree, mode: EntryMode::Tree });
    let tree = s.build_tree(&outer).unwrap();
    let gs = s.subtree(tree, "gitstate").unwrap().unwrap();
    assert_eq!(s.subtree(gs, "index").unwrap(), Some(inner_tree));
    assert_eq!(s.read_tree(tree).unwrap().get(&RelPath::new("gitstate/index/x")).unwrap().mode, EntryMode::Blob);
}

#[test]
fn empty_workmap_builds_empty_tree() {
    let (_d, s) = store();
    let t = s.build_tree(&WorkMap::default()).unwrap();
    assert_eq!(t.to_string(), "4b825dc642cb6eb9a060e54bf8d69288fbee4904");
    assert!(s.read_tree(t).unwrap().is_empty());
}

#[test]
fn write_blob_streams_large_file() {
    let (d, s) = store();
    let path = d.path().join("big.bin");
    let mut f = std::fs::File::create(&path).unwrap();
    let mut chunk = vec![0u8; 1 << 20];
    for i in 0..64u8 {
        chunk.iter_mut().enumerate().for_each(|(j, b)| *b = (j as u8) ^ i);
        f.write_all(&chunk).unwrap();
    }
    drop(f);
    let size = std::fs::metadata(&path).unwrap().len();
    let oid = s.write_blob_stream(&mut std::fs::File::open(&path).unwrap(), size).unwrap();
    assert_eq!(oid, git2::Oid::hash_file(git2::ObjectType::Blob, &path).unwrap());
    assert_eq!(s.blob_size(oid).unwrap(), size);
    let mut back = Vec::new();
    assert_eq!(s.copy_blob_to(oid, &mut back).unwrap(), size);
    assert_eq!(back.len() as u64, size);
    assert_eq!(back, std::fs::read(&path).unwrap());
    assert!(s.write_blob_stream(&mut &b"short"[..], 10).is_err());
}

#[test]
fn same_content_same_oid_and_raw_roundtrip() {
    let (_d, s) = store();
    let a = s.write_blob(b"hello").unwrap();
    assert_eq!(a, s.write_blob(b"hello").unwrap());
    assert_eq!(s.read_blob(a).unwrap(), b"hello");
    assert!(s.has(a));
    let (kind, raw) = s.read_raw(a).unwrap().unwrap();
    assert_eq!(kind, git2::ObjectType::Blob);
    let (_d2, other) = store();
    assert!(!other.has(a));
    assert_eq!(other.write_raw(kind, &raw).unwrap(), a);
    assert!(other.read_raw(git2::Oid::ZERO_SHA1).unwrap().is_none());
}

#[test]
fn commit_has_parents_and_time() {
    let (_d, s) = store();
    let t = s.build_tree(&WorkMap::default()).unwrap();
    let c1 = s.commit(t, &[], "first", 100).unwrap();
    let c2 = s.commit(t, &[c1], "second", 200).unwrap();
    let c = s.repo().find_commit(c2).unwrap();
    assert_eq!(c.parent_ids().collect::<Vec<_>>(), vec![c1]);
    assert_eq!(c.time().seconds(), 200);
    assert_eq!(c.message().unwrap(), "second");
    assert_eq!(s.commit_tree(c2).unwrap(), t);
}

#[test]
fn reopen_existing_store() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("store.git");
    let oid = Store::open_or_init(&p).unwrap().write_blob(b"x").unwrap();
    assert!(Store::open_or_init(&p).unwrap().has(oid));
    std::fs::write(d.path().join("file"), b"").unwrap();
    assert!(Store::open_or_init(&d.path().join("file")).is_err());
}
