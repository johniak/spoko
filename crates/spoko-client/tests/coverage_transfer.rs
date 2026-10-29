//! Edge cases of the encrypted object transfer: codec framing, the known set, malformed
//! objects, tampered server data, batch splitting and server failures.
#![cfg_attr(windows, allow(unused_imports, dead_code))] // helpers of #[cfg(unix)] tests

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use git2::{ObjectType, Oid};
use rand::RngCore;
use spoko_client::ClientError;
use spoko_client::test_util::MemServer;
use spoko_client::transfer::chunks::{
    CHUNK_THRESHOLD, Manifest, fetch_stream, parallel_get, parallel_put, push_stream,
};
use spoko_client::transfer::codec::{
    self, KIND_BLOB, KIND_CHUNK, KIND_MANIFEST, KIND_TAG, seal_object,
};
use spoko_client::transfer::download::fetch_closure;
use spoko_client::transfer::enumerate::{Role, deps, new_objects, tree_entries};
use spoko_client::transfer::known::KnownSet;
use spoko_client::transfer::upload::push_objects;
use spoko_client::transfer::{ObjectServer, WORKERS};
use spoko_core::platform::RelPath;
use spoko_core::snapshot::{SnapshotMeta, SnapshotParts, write_snapshot};
use spoko_core::store::{Entry, EntryMode, Store, WorkMap};
use spoko_crypto::{Keyring, ObjectId, chunk_id, git_object_id};

struct Side {
    _d: tempfile::TempDir,
    store: Store,
    known: KnownSet,
}

fn side() -> Side {
    let d = tempfile::tempdir().unwrap();
    let store = Store::open_or_init(&d.path().join("s.git")).unwrap();
    let known = KnownSet::load(&d.path().join("known.bin")).unwrap();
    Side {
        _d: d,
        store,
        known,
    }
}

/// A snapshot whose work tree holds `files` (blobs) and `links` (gitlinks, mode 160000).
fn snapshot(s: &Store, files: &[(&str, Vec<u8>)], links: &[(&str, Oid)]) -> Oid {
    let mut w = WorkMap::default();
    for (p, c) in files {
        let oid = s.write_blob(c).unwrap();
        w.insert(
            RelPath::new(p),
            Entry {
                oid,
                mode: EntryMode::Blob,
            },
        );
    }
    for (p, oid) in links {
        w.insert(
            RelPath::new(p),
            Entry {
                oid: *oid,
                mode: EntryMode::Commit,
            },
        );
    }
    let parts = SnapshotParts {
        work: s.build_tree(&w).unwrap(),
        gitstate: None,
        externals: BTreeMap::new(),
        meta: SnapshotMeta {
            device_id: uuid::Uuid::nil(),
            device_name: "a".into(),
            time: 1,
            conflicts: vec![],
        },
    };
    write_snapshot(s, &parts, &[]).unwrap()
}

fn push_to(a: &mut Side, srv: &dyn ObjectServer, kr: &Keyring, snap: Oid) -> UploadResult {
    let objs = new_objects(&a.store, &[snap], &a.known, kr).unwrap();
    push_objects(&a.store, srv, kr, &mut a.known, &objs)
}

type UploadResult = spoko_client::Result<spoko_client::transfer::upload::UploadStats>;

/// An object server that records its traffic and can fail uploads.
#[derive(Default)]
struct Probe {
    inner: MemServer,
    /// Keep uploaded objects (off for tests that only look at batch sizes).
    keep: bool,
    fail_put: bool,
    put_batches: Mutex<Vec<(usize, usize)>>,
    get_calls: AtomicUsize,
}

impl Probe {
    fn keeping() -> Probe {
        Probe {
            keep: true,
            ..Probe::default()
        }
    }
    fn put_calls(&self) -> usize {
        self.put_batches.lock().unwrap().len()
    }
}

impl ObjectServer for Probe {
    fn missing(&self, ids: &[ObjectId]) -> spoko_client::Result<Vec<ObjectId>> {
        self.inner.missing(ids)
    }
    fn put(&self, batch: &[(ObjectId, Vec<u8>)]) -> spoko_client::Result<()> {
        if self.fail_put {
            return Err(ClientError::Server("disk full".into()));
        }
        let bytes = batch.iter().map(|(_, b)| b.len() + 36).sum();
        self.put_batches.lock().unwrap().push((batch.len(), bytes));
        if self.keep {
            self.inner.put(batch)
        } else {
            Ok(())
        }
    }
    fn get(&self, ids: &[ObjectId]) -> spoko_client::Result<Vec<(ObjectId, Vec<u8>)>> {
        self.get_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.get(ids)
    }
}

fn random(n: usize) -> Vec<u8> {
    let mut v = vec![0; n];
    rand::thread_rng().fill_bytes(&mut v);
    v
}

fn integrity_message(r: spoko_client::Result<impl std::fmt::Debug>) -> String {
    match r {
        Err(ClientError::Integrity(m)) => m,
        other => panic!("expected an integrity error, got {other:?}"),
    }
}

#[test]
fn codec_maps_tags_and_rejects_unknown_or_short_objects() {
    assert_eq!(codec::kind_of(ObjectType::Tag), KIND_TAG);
    assert_eq!(codec::object_type(KIND_TAG).unwrap(), ObjectType::Tag);
    let m = integrity_message(codec::object_type(KIND_MANIFEST));
    assert_eq!(m, "not a git object kind: 5");
    assert_eq!(
        integrity_message(codec::decode(&[KIND_BLOB])),
        "short object"
    );
    assert_eq!(integrity_message(codec::decode(&[])), "short object");
    assert_eq!(codec::decode(&[KIND_BLOB, 0]).unwrap(), (KIND_BLOB, vec![]));
}

#[test]
fn in_memory_known_set_tracks_ids_without_a_file() {
    let mut k = KnownSet::in_memory();
    assert!(k.is_empty());
    k.insert_many([ObjectId([1; 32]), ObjectId([2; 32]), ObjectId([1; 32])])
        .unwrap();
    assert!(!k.is_empty());
    assert_eq!(k.len(), 2);
    assert!(k.contains(&ObjectId([2; 32])));
    k.clear().unwrap();
    assert!(k.is_empty());
    assert!(!k.contains(&ObjectId([2; 32])));
}

#[test]
fn known_set_file_is_created_on_insert_and_removed_on_clear() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("nested/dir/known.bin");
    let mut k = KnownSet::load(&path).unwrap();
    assert!(k.is_empty());
    k.insert_many([ObjectId([7; 32]), ObjectId([8; 32])])
        .unwrap();
    assert_eq!(std::fs::read(&path).unwrap().len(), 64);
    // re-inserting known ids appends nothing
    k.insert_many([ObjectId([7; 32])]).unwrap();
    assert_eq!(std::fs::read(&path).unwrap().len(), 64);
    let reloaded = KnownSet::load(&path).unwrap();
    assert_eq!(reloaded.len(), 2);
    assert!(reloaded.contains(&ObjectId([8; 32])));
    k.clear().unwrap();
    assert!(!path.exists());
    assert!(k.is_empty());
    k.clear().unwrap(); // nothing to remove: still fine
    assert!(KnownSet::load(&path).unwrap().is_empty());
}

#[test]
fn known_set_reports_unreadable_and_unwritable_files() {
    let d = tempfile::tempdir().unwrap();
    let file = d.path().join("plain-file");
    std::fs::write(&file, b"x").unwrap();
    let err = KnownSet::load(&file.join("known.bin")).unwrap_err();
    assert_ne!(err.kind(), std::io::ErrorKind::NotFound);

    // the directory for the file cannot be created once a file took its name
    let path = d.path().join("later/known.bin");
    let mut k = KnownSet::load(&path).unwrap();
    std::fs::write(d.path().join("later"), b"x").unwrap();
    assert!(k.insert_many([ObjectId([1; 32])]).is_err());
    assert!(!path.exists());

    // a path without a parent directory cannot be opened at all
    let mut nameless = KnownSet::load(std::path::Path::new("")).unwrap();
    assert!(nameless.insert_many([ObjectId([1; 32])]).is_err());
}

#[test]
fn tree_entries_stop_at_truncated_data() {
    let oid = Oid::hash_object(ObjectType::Blob, b"x").unwrap();
    let mut good = b"100644 a\0".to_vec();
    good.extend_from_slice(oid.as_bytes());
    assert_eq!(tree_entries(&good), vec![(0o100644, oid)]);
    assert!(tree_entries(b"100644 no-terminator").is_empty());
    assert!(tree_entries(b"100644 short\0abc").is_empty());
    let mut tail = good.clone();
    tail.extend_from_slice(b"40000 cut\0xyz");
    assert_eq!(
        tree_entries(&tail),
        vec![(0o100644, oid)],
        "the valid prefix is kept"
    );
}

#[test]
fn tag_dependencies_point_at_the_tagged_object() {
    let target = Oid::hash_object(ObjectType::Blob, b"t").unwrap();
    let tag = format!("object {target}\ntype commit\ntag v1\n\nmsg\n");
    assert_eq!(
        deps(ObjectType::Tag, Role::Any, tag.as_bytes()),
        vec![(target, Role::Any, false)]
    );
    assert!(deps(ObjectType::Tag, Role::Any, b"tag v1\n\nno object line\n").is_empty());
    assert!(deps(ObjectType::Tag, Role::Any, b"object not-hex\n").is_empty());
}

#[test]
fn gitlinked_annotated_tag_brings_its_commit() {
    let kr = Keyring::generate();
    let srv = MemServer::default();
    let mut a = side();
    let b = side();
    let tree = a.store.build_tree(&WorkMap::default()).unwrap();
    let commit = a.store.commit(tree, &[], "user work", 1).unwrap();
    let raw = format!(
        "object {commit}\ntype commit\ntag v1\ntagger Jan <jan@example.com> 1 +0000\n\nrelease\n"
    );
    let tag = a.store.write_raw(ObjectType::Tag, raw.as_bytes()).unwrap();
    let snap = snapshot(&a.store, &[("README", b"hi".to_vec())], &[("sub", tag)]);
    push_to(&mut a, &srv, &kr, snap).unwrap();
    let stats = fetch_closure(&b.store, &srv, &kr, &[snap]).unwrap();
    assert!(b.store.has(tag) && b.store.has(commit) && b.store.has(tree));
    assert_eq!(
        b.store.read_raw(tag).unwrap().unwrap(),
        (ObjectType::Tag, raw.into_bytes())
    );
    assert_eq!(stats.objects, stats.ids.len());
}

#[test]
fn gitlink_target_missing_everywhere_is_skipped() {
    let kr = Keyring::generate();
    let srv = MemServer::default();
    let mut a = side();
    let b = side();
    let absent = Oid::hash_object(ObjectType::Commit, b"never written").unwrap();
    let snap = snapshot(&a.store, &[("f", b"1".to_vec())], &[("vendor/lib", absent)]);
    push_to(&mut a, &srv, &kr, snap).unwrap();
    fetch_closure(&b.store, &srv, &kr, &[snap]).unwrap();
    assert!(b.store.has(snap));
    assert!(!b.store.has(absent));
}

#[test]
fn oversized_commit_is_chunked_and_reassembled() {
    let kr = Keyring::generate();
    let srv = MemServer::default();
    let mut a = side();
    let b = side();
    let tree = a.store.build_tree(&WorkMap::default()).unwrap();
    let message: String = (0..CHUNK_THRESHOLD as usize / 8 + 1000)
        .map(|i| format!("{i:07}\n"))
        .collect();
    let commit = a.store.commit(tree, &[], &message, 1).unwrap();
    let snap = snapshot(&a.store, &[], &[("sub", commit)]);
    let up = push_to(&mut a, &srv, &kr, snap).unwrap();
    assert!(up.chunks >= 1, "the commit went up as chunks: {up:?}");
    fetch_closure(&b.store, &srv, &kr, &[snap]).unwrap();
    let got = b.store.repo().find_commit(commit).unwrap();
    assert_eq!(got.message_bytes(), message.as_bytes());
}

#[test]
fn object_with_substituted_content_is_rejected() {
    let kr = Keyring::generate();
    let srv = MemServer::default();
    let b = side();
    let oid = Oid::hash_object(ObjectType::Blob, b"real").unwrap();
    let id = git_object_id(&kr, oid.as_bytes());
    srv.objects
        .lock()
        .unwrap()
        .insert(id, seal_object(&kr, &id, KIND_BLOB, b"fake"));
    let m = integrity_message(fetch_closure(&b.store, &srv, &kr, &[oid]));
    assert_eq!(m, format!("object {oid} content mismatch"));
    assert!(!b.store.has(oid));
}

#[test]
fn chunk_with_substituted_content_is_rejected() {
    let kr = Keyring::generate();
    let srv = MemServer::default();
    let id = chunk_id(&kr, b"real");
    srv.objects
        .lock()
        .unwrap()
        .insert(id, seal_object(&kr, &id, KIND_CHUNK, b"fake"));
    let m = Manifest {
        kind: KIND_BLOB,
        size: 4,
        chunks: vec![id],
    };
    let mut out = Vec::new();
    let err = integrity_message(fetch_stream(&srv, &kr, &m, &mut out));
    assert_eq!(err, format!("chunk {id} content mismatch"));
    assert!(out.is_empty(), "nothing of a bad chunk is written");
}

#[test]
fn chunked_blob_reassembling_to_other_content_is_rejected() {
    let kr = Keyring::generate();
    let srv = MemServer::default();
    let b = side();
    let (data, other) = (random(300 << 10), random(300 << 10));
    let (manifest, _) = push_stream(&srv, &kr, &data[..], KIND_BLOB).unwrap();
    let oid = Oid::hash_object(ObjectType::Blob, &other).unwrap();
    let id = git_object_id(&kr, oid.as_bytes());
    let sealed = seal_object(&kr, &id, KIND_MANIFEST, &manifest.to_bytes());
    srv.objects.lock().unwrap().insert(id, sealed);
    let m = integrity_message(fetch_closure(&b.store, &srv, &kr, &[oid]));
    assert_eq!(m, format!("object {oid} content mismatch"));
    assert!(!b.store.has(oid));
}

#[test]
fn worker_batches_are_split_below_the_server_limit() {
    let kr = Keyring::generate();
    let srv = Probe::default();
    let size = spoko_proto::MAX_BATCH_BYTES / 2 + (1 << 20);
    let items: Vec<_> = (0..=WORKERS as u8)
        .map(|i| (ObjectId([i; 32]), KIND_BLOB, random(size)))
        .collect();
    let sent = parallel_put(&srv, &kr, items).unwrap();
    assert!(sent > (WORKERS + 1) * size, "{sent}");
    let batches = srv.put_batches.lock().unwrap().clone();
    assert_eq!(
        batches.len(),
        WORKERS + 1,
        "one worker got two objects and sent them apart"
    );
    assert!(
        batches
            .iter()
            .all(|(n, bytes)| *n == 1 && *bytes <= spoko_proto::MAX_BATCH_BYTES)
    );
}

#[test]
fn failed_upload_is_reported_and_nothing_becomes_known() {
    let kr = Keyring::generate();
    let srv = Probe {
        fail_put: true,
        ..Probe::default()
    };
    let mut a = side();
    let snap = snapshot(&a.store, &[("f", b"1".to_vec())], &[]);
    let err = push_to(&mut a, &srv, &kr, snap).unwrap_err();
    assert!(
        matches!(&err, ClientError::Server(m) if m == "disk full"),
        "{err}"
    );
    assert!(a.known.is_empty());
}

#[test]
fn fetching_no_ids_asks_the_server_nothing() {
    let srv = Probe::default();
    assert!(
        parallel_get(&srv, &Keyring::generate(), &[])
            .unwrap()
            .is_empty()
    );
    assert_eq!(srv.get_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn objects_already_on_the_server_are_not_uploaded_again() {
    let kr = Keyring::generate();
    let srv = MemServer::default();
    let mut a = side();
    let snap = snapshot(&a.store, &[("f", b"1".to_vec()), ("g", b"2".to_vec())], &[]);
    let first = push_to(&mut a, &srv, &kr, snap).unwrap();
    assert!(first.objects > 0);
    let puts = srv.puts();
    // same store, but a fresh device-side record of what the server has
    let mut fresh = KnownSet::in_memory();
    let objs = new_objects(&a.store, &[snap], &fresh, &kr).unwrap();
    let again = push_objects(&a.store, &srv, &kr, &mut fresh, &objs).unwrap();
    assert_eq!((again.objects, again.bytes), (0, 0));
    assert_eq!(srv.puts(), puts);
    assert_eq!(
        fresh.len(),
        objs.len(),
        "everything is now known to be on the server"
    );
}

#[test]
fn many_small_objects_are_flushed_in_several_rounds() {
    let kr = Keyring::generate();
    let srv = Probe::keeping();
    let mut a = side();
    let b = side();
    let files: Vec<(String, Vec<u8>)> = (0..4200)
        .map(|i| (format!("d/f{i}"), format!("{i}").into_bytes()))
        .collect();
    let refs: Vec<(&str, Vec<u8>)> = files.iter().map(|(p, c)| (p.as_str(), c.clone())).collect();
    let snap = snapshot(&a.store, &refs, &[]);
    let stats = push_to(&mut a, &srv, &kr, snap).unwrap();
    assert!(stats.objects > 4200, "{stats:?}");
    assert!(
        srv.put_calls() > WORKERS,
        "a second flush ran: {}",
        srv.put_calls()
    );
    fetch_closure(&b.store, &srv, &kr, &[snap]).unwrap();
    let blob = Oid::hash_object(ObjectType::Blob, b"4199").unwrap();
    assert_eq!(b.store.read_blob(blob).unwrap(), b"4199");
}
