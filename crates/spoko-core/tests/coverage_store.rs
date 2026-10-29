#![cfg_attr(windows, allow(unused_imports, dead_code))] // helpers of #[cfg(unix)] tests
use std::io::{self, Read, Write};

use git2::{ObjectType, Oid};
use spoko_core::platform::RelPath;
use spoko_core::store::{Entry, EntryMode, Store, WorkMap};

fn store() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::open_or_init(&d.path().join("store.git")).unwrap();
    (d, s)
}

fn pack_files(s: &Store) -> usize {
    std::fs::read_dir(s.repo().path().join("objects/pack")).map_or(0, |rd| rd.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "pack")).count())
}

struct Failing;

impl Read for Failing {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::other("disk gone"))
    }
}

impl Write for Failing {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("pipe closed"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn batched_stream_write_reports_reader_error() {
    let (_d, s) = store();
    let r: Result<Oid, git2::Error> = s.batch(|| s.write_blob_stream(&mut Failing, 10));
    assert!(r.unwrap_err().message().contains("disk gone"));
}

#[test]
fn batched_stream_write_rejects_short_reader() {
    let (_d, s) = store();
    let r: Result<Oid, git2::Error> = s.batch(|| s.write_blob_stream(&mut &b"short"[..], 10));
    assert_eq!(r.unwrap_err().message(), "file changed while reading");
}

#[test]
fn pending_objects_are_readable_inside_batch() {
    let (_d, s) = store();
    s.batch(|| -> Result<(), git2::Error> {
        let oid = s.write_blob(b"hello")?;
        assert!(s.has(oid));
        assert_eq!(s.blob_size(oid)?, 5);
        let mut out = Vec::new();
        assert_eq!(s.copy_blob_to(oid, &mut out)?, 5);
        assert_eq!(out, b"hello");
        assert_eq!(s.read_raw(oid)?, Some((ObjectType::Blob, b"hello".to_vec())));
        assert!(s.copy_blob_to(oid, &mut Failing).unwrap_err().message().contains("pipe closed"));
        assert_eq!(std::fs::read_dir(s.repo().path().join("objects")).unwrap().flatten().filter(|e| e.file_name().len() == 2).count(), 0, "nothing on disk yet");
        Ok(())
    })
    .unwrap();
    assert_eq!(s.read_blob(git2::Oid::hash_object(ObjectType::Blob, b"hello").unwrap()).unwrap(), b"hello");
}

#[test]
fn batch_flushes_a_pack_once_64_mib_is_buffered() {
    let (_d, s) = store();
    let oids = s
        .batch(|| -> Result<Vec<Oid>, git2::Error> {
            let mut oids = Vec::new();
            for i in 0..64u8 {
                oids.push(s.write_blob(&vec![i; 1 << 20])?);
                if i < 63 {
                    assert_eq!(pack_files(&s), 0, "still buffered after {} MiB", i + 1);
                }
            }
            assert_eq!(pack_files(&s), 1, "flushed mid-batch");
            assert_eq!(s.blob_size(oids[0])?, 1 << 20, "flushed objects stay readable");
            Ok(oids)
        })
        .unwrap();
    assert_eq!(pack_files(&s), 1, "nothing left to pack at the end");
    for (i, oid) in oids.iter().enumerate() {
        let data = s.read_blob(*oid).unwrap();
        assert_eq!(data.len(), 1 << 20);
        assert!(data.iter().all(|b| usize::from(*b) == i));
    }
}

#[test]
fn packed_batch_keeps_commit_and_tag_kinds() {
    let (_d, s) = store();
    let empty = s.build_tree(&WorkMap::default()).unwrap();
    let commit = format!("tree {empty}\nauthor a <a@b> 1 +0000\ncommitter a <a@b> 1 +0000\n\nmsg\n").into_bytes();
    let commit_oid = Oid::hash_object(ObjectType::Commit, &commit).unwrap();
    let tag = format!("object {commit_oid}\ntype commit\ntag v1\ntagger a <a@b> 1 +0000\n\nrelease\n").into_bytes();
    let tag_oid = s
        .batch(|| -> Result<Oid, git2::Error> {
            for i in 0..64u32 {
                s.write_blob(&i.to_be_bytes())?;
            }
            assert_eq!(s.write_raw(ObjectType::Commit, &commit)?, commit_oid);
            s.write_raw(ObjectType::Tag, &tag)
        })
        .unwrap();
    assert_eq!(pack_files(&s), 1);
    assert_eq!(s.read_raw(commit_oid).unwrap(), Some((ObjectType::Commit, commit)));
    assert_eq!(s.read_raw(tag_oid).unwrap(), Some((ObjectType::Tag, tag)));
    assert_eq!(s.repo().find_tag(tag_oid).unwrap().target_id(), commit_oid);
    assert_eq!(s.commit_tree(commit_oid).unwrap(), empty);
}

#[test]
fn corrupt_loose_object_is_an_error_not_a_miss() {
    let (_d, s) = store();
    let oid = s.write_blob(b"precious").unwrap();
    let hex = oid.to_string();
    let path = s.repo().path().join("objects").join(&hex[..2]).join(&hex[2..]);
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);
    std::fs::set_permissions(&path, perms).unwrap();
    std::fs::write(&path, b"this is not zlib").unwrap();
    assert!(s.read_raw(oid).is_err());
    let missing = Oid::from_str("1234567890123456789012345678901234567890").unwrap();
    assert_eq!(s.read_raw(missing).unwrap(), None);
}

/// libgit2 normalizes tree entry modes, so an odd mode (here a socket) reads back as a blob.
#[test]
fn tree_entries_with_odd_modes_read_as_blobs() {
    let (_d, s) = store();
    let blob = s.write_blob(b"x").unwrap();
    let mut raw = Vec::new();
    raw.extend_from_slice(b"100644 a\0");
    raw.extend_from_slice(blob.as_bytes());
    raw.extend_from_slice(b"140000 sock\0");
    raw.extend_from_slice(blob.as_bytes());
    let tree = s.write_raw(ObjectType::Tree, &raw).unwrap();
    let w = s.read_tree(tree).unwrap();
    assert_eq!(w.keys().map(RelPath::as_str).collect::<Vec<_>>(), ["a", "sock"]);
    assert!(w.values().all(|e| e.mode == EntryMode::Blob && e.oid == blob));
}

#[test]
fn unbatched_stream_write_reports_reader_error() {
    let (_d, s) = store();
    assert!(s.write_blob_stream(&mut Failing, 10).unwrap_err().message().contains("disk gone"));
}

#[test]
fn copying_loose_and_packed_blobs_reports_writer_error() {
    let (_d, s) = store();
    let loose = s.write_blob(b"loose").unwrap();
    let packed = s
        .batch(|| -> Result<Oid, git2::Error> {
            for i in 0..64u32 {
                s.write_blob(&i.to_be_bytes())?;
            }
            s.write_blob(b"packed")
        })
        .unwrap();
    assert_eq!(pack_files(&s), 1);
    for oid in [loose, packed] {
        assert!(s.copy_blob_to(oid, &mut Failing).unwrap_err().message().contains("pipe closed"));
        let mut out = Vec::new();
        s.copy_blob_to(oid, &mut out).unwrap();
        assert_eq!(out, s.read_blob(oid).unwrap());
    }
}

#[test]
fn reading_a_tree_with_a_missing_subtree_fails() {
    let (_d, s) = store();
    let mut w = WorkMap::default();
    w.insert(RelPath::new("d"), Entry { oid: Oid::from_bytes(&[9; 20]).unwrap(), mode: EntryMode::Tree });
    let tree = s.build_tree(&w).unwrap();
    assert_eq!(s.read_tree(tree).unwrap_err().code(), git2::ErrorCode::NotFound);
}

#[test]
fn forgetting_remembered_trees_keeps_trees_correct_and_deduplicated() {
    let d = tempfile::tempdir().unwrap();
    let mut store = spoko_core::store::Store::open_or_init(&d.path().join("s.git")).unwrap();
    store.limit_tree_cache(2);
    let mut w = spoko_core::store::WorkMap::default();
    for i in 0..5 {
        let oid = store.write_blob(format!("{i}").as_bytes()).unwrap();
        w.insert(spoko_core::platform::RelPath::new(&format!("d{i}/f")), spoko_core::store::Entry { oid, mode: spoko_core::store::EntryMode::Blob });
    }
    let first = store.build_tree(&w).unwrap();
    let objects = || walkdir_count(&d.path().join("s.git/objects"));
    let before = objects();
    assert_eq!(store.build_tree(&w).unwrap(), first, "6 trees through a cache of 2: same root");
    assert_eq!(objects(), before, "nothing rewritten after the cache was emptied");
    assert_eq!(store.read_tree(first).unwrap(), w);
}

fn walkdir_count(p: &std::path::Path) -> usize {
    std::fs::read_dir(p).unwrap().filter_map(Result::ok).map(|e| if e.path().is_dir() { walkdir_count(&e.path()) } else { 1 }).sum()
}
