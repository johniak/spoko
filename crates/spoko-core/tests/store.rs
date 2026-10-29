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

#[test]
fn batch_writes_one_pack_and_objects_stay_readable() {
    let d = tempfile::tempdir().unwrap();
    let store = Store::open_or_init(&d.path().join("s.git")).unwrap();
    let loose_before = count_loose(&d.path().join("s.git/objects"));
    let ids = store
        .batch(|| -> Result<Vec<git2::Oid>, git2::Error> {
            let mut ids = vec![];
            for i in 0..500 {
                let oid = store.write_blob(format!("blob {i}").as_bytes())?;
                assert!(store.has(oid), "pending objects are visible inside the batch");
                assert_eq!(store.read_blob(oid)?, format!("blob {i}").into_bytes());
                ids.push(oid);
            }
            ids.push(store.write_blob(b"blob 0")?); // duplicate
            ids.push(store.write_blob_stream(&mut &b"streamed"[..], 8)?);
            let big = vec![7u8; (1 << 20) + 1];
            let b = store.write_blob(&big)?;
            assert!(store.repo().odb()?.reader(b).is_ok(), "big blobs are loose and streamable");
            Ok(ids)
        })
        .unwrap();
    assert_eq!(count_loose(&d.path().join("s.git/objects")), loose_before + 1, "only the big blob is loose");
    let packs = std::fs::read_dir(d.path().join("s.git/objects/pack")).unwrap().filter(|e| e.as_ref().unwrap().path().extension().is_some_and(|x| x == "pack")).count();
    assert_eq!(packs, 1);
    assert!(ids.iter().all(|o| store.has(*o)), "the same handle sees its own pack without a lookup refresh");
    for (i, oid) in ids.iter().take(500).enumerate() {
        assert_eq!(store.read_blob(*oid).unwrap(), format!("blob {i}").into_bytes());
        let mut out = vec![];
        store.copy_blob_to(*oid, &mut out).unwrap();
        assert_eq!(out, format!("blob {i}").into_bytes(), "packed blobs can be copied out");
        assert_eq!(store.blob_size(*oid).unwrap(), out.len() as u64);
    }
    assert_eq!(store.read_blob(ids[501]).unwrap(), b"streamed");
    // a fresh handle sees the pack too
    let again = Store::open_or_init(&d.path().join("s.git")).unwrap();
    assert!(again.has(ids[42]));
    // a tree over packed blobs builds and reads back
    let mut w = WorkMap::default();
    w.insert(RelPath::new("a.txt"), Entry { oid: ids[3], mode: EntryMode::Blob });
    let t = store.build_tree(&w).unwrap();
    assert_eq!(store.read_tree(t).unwrap(), w);
}

#[test]
fn small_batches_stay_loose_and_errors_still_flush() {
    let d = tempfile::tempdir().unwrap();
    let store = Store::open_or_init(&d.path().join("s.git")).unwrap();
    let r: Result<(), git2::Error> = store.batch(|| {
        store.write_blob(b"kept")?;
        Err(git2::Error::from_str("boom"))
    });
    assert!(r.is_err());
    let oid = git2::Oid::hash_object(git2::ObjectType::Blob, b"kept").unwrap();
    assert!(store.has(oid), "written objects survive a failed batch (stat cache may point at them)");
    assert!(!d.path().join("s.git/objects/pack").read_dir().unwrap().any(|e| e.unwrap().path().extension().is_some_and(|x| x == "pack")), "tiny batch → loose");
    // nested batches flush once, at the outer end
    store.batch(|| store.batch(|| store.write_raw(git2::ObjectType::Blob, b"nested").map(|_| ()))).unwrap();
    assert!(store.has(git2::Oid::hash_object(git2::ObjectType::Blob, b"nested").unwrap()));
}

fn count_loose(objects: &std::path::Path) -> usize {
    std::fs::read_dir(objects).unwrap().filter_map(Result::ok).filter(|e| e.file_name().len() == 2).map(|e| std::fs::read_dir(e.path()).unwrap().count()).sum()
}

#[test]
fn build_tree_matches_git_treebuilder_and_packs() {
    let d = tempfile::tempdir().unwrap();
    let store = Store::open_or_init(&d.path().join("s.git")).unwrap();
    let blob = store.write_blob(b"x").unwrap();
    let link = store.write_blob(b"target").unwrap();
    let mut w = WorkMap::default();
    // git sorts "a-b" < "a/" < "a0" (directories compare as "name/")
    for (p, mode) in [("a-b", EntryMode::Blob), ("a/x", EntryMode::Blob), ("a0", EntryMode::BlobExec), ("ln", EntryMode::Link), ("sub", EntryMode::Commit), ("z/y/q", EntryMode::Blob)] {
        let oid = match mode {
            EntryMode::Link => link,
            EntryMode::Commit => git2::Oid::from_str("0123456789abcdef0123456789abcdef01234567").unwrap(),
            _ => blob,
        };
        w.insert(RelPath::new(p), Entry { oid, mode });
    }
    // reference: libgit2's treebuilder
    let repo = store.repo();
    let tb_leaf = |entries: &[(&str, git2::Oid, i32)]| {
        let mut tb = repo.treebuilder(None).unwrap();
        for (n, o, m) in entries {
            tb.insert(n, *o, *m).unwrap();
        }
        tb.write().unwrap()
    };
    let a = tb_leaf(&[("x", blob, 0o100644)]);
    let y = tb_leaf(&[("q", blob, 0o100644)]);
    let z = tb_leaf(&[("y", y, 0o040000)]);
    git2::opts::strict_object_creation(false);
    let reference = tb_leaf(&[("a-b", blob, 0o100644), ("a", a, 0o040000), ("a0", blob, 0o100755), ("ln", link, 0o120000), ("sub", w[&RelPath::new("sub")].oid, 0o160000), ("z", z, 0o040000)]);
    git2::opts::strict_object_creation(true);
    assert_eq!(store.build_tree(&w).unwrap(), reference);
    assert_eq!(store.read_tree(reference).unwrap(), w);
    assert_eq!(store.build_tree(&WorkMap::default()).unwrap().to_string(), "4b825dc642cb6eb9a060e54bf8d69288fbee4904", "empty tree");
}

#[test]
fn big_tree_is_packed_not_loose() {
    let d = tempfile::tempdir().unwrap();
    let store = Store::open_or_init(&d.path().join("s.git")).unwrap();
    let mut w = WorkMap::default();
    for i in 0..200 {
        let blob = store.write_blob(format!("x{i}").as_bytes()).unwrap();
        w.insert(RelPath::new(&format!("d{i}/f")), Entry { oid: blob, mode: EntryMode::Blob });
    }
    let before = count_loose(&d.path().join("s.git/objects"));
    let t = store.build_tree(&w).unwrap();
    assert_eq!(count_loose(&d.path().join("s.git/objects")), before, "201 trees → one pack");
    assert_eq!(store.read_tree(t).unwrap(), w);
    assert_eq!(store.build_tree(&w).unwrap(), t, "rebuild is stable");
}
