use std::collections::BTreeMap;

use git2::Oid;
use rand::RngCore;
use spoko_client::test_util::MemServer;
use spoko_client::transfer::download::fetch_closure;
use spoko_client::transfer::enumerate::new_objects;
use spoko_client::transfer::known::KnownSet;
use spoko_client::transfer::upload::push_objects;
use spoko_core::platform::RelPath;
use spoko_core::snapshot::{SnapshotMeta, SnapshotParts, write_snapshot};
use spoko_core::store::{Entry, EntryMode, Store, WorkMap};
use spoko_crypto::Keyring;

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

fn snapshot(s: &Store, files: &[(&str, Vec<u8>)], parents: &[Oid], gitstate: Option<Oid>) -> Oid {
    let mut w = WorkMap::default();
    for (p, c) in files {
        w.insert(
            RelPath::new(p),
            Entry {
                oid: s.write_blob(c).unwrap(),
                mode: EntryMode::Blob,
            },
        );
    }
    let parts = SnapshotParts {
        work: s.build_tree(&w).unwrap(),
        gitstate,
        externals: BTreeMap::new(),
        meta: SnapshotMeta {
            device_id: uuid::Uuid::nil(),
            device_name: "a".into(),
            time: 1,
            conflicts: vec![],
        },
    };
    write_snapshot(s, &parts, parents).unwrap()
}

fn push(
    a: &mut Side,
    srv: &MemServer,
    kr: &Keyring,
    snap: Oid,
) -> spoko_client::transfer::upload::UploadStats {
    let objs = new_objects(&a.store, &[snap], &a.known, kr).unwrap();
    push_objects(&a.store, srv, kr, &mut a.known, &objs).unwrap()
}

#[test]
fn roundtrip_snapshot_between_stores() {
    let kr = Keyring::generate();
    let srv = MemServer::default();
    let mut a = side();
    let b = side();
    let snap = snapshot(
        &a.store,
        &[
            ("src/a.rs", b"fn a(){}".to_vec()),
            ("README", b"hi".to_vec()),
        ],
        &[],
        None,
    );
    push(&mut a, &srv, &kr, snap);
    fetch_closure(&b.store, &srv, &kr, &[snap]).unwrap();
    let tree = b.store.commit_tree(snap).unwrap();
    let work = b
        .store
        .read_tree(b.store.subtree(tree, "work").unwrap().unwrap())
        .unwrap();
    assert_eq!(
        b.store
            .read_blob(work[&RelPath::new("src/a.rs")].oid)
            .unwrap(),
        b"fn a(){}"
    );
    assert_eq!(
        fetch_closure(&b.store, &srv, &kr, &[snap]).unwrap().objects,
        0,
        "already present"
    );
}

#[test]
fn second_push_uploads_only_changed() {
    let kr = Keyring::generate();
    let srv = MemServer::default();
    let mut a = side();
    let files: Vec<(String, Vec<u8>)> = (0..100)
        .map(|i| {
            (
                format!("d{}/f{i}", i % 10),
                format!("content {i}").into_bytes(),
            )
        })
        .collect();
    let refs: Vec<(&str, Vec<u8>)> = files.iter().map(|(p, c)| (p.as_str(), c.clone())).collect();
    let s1 = snapshot(&a.store, &refs, &[], None);
    push(&mut a, &srv, &kr, s1);
    srv.reset_counters();
    let mut changed = refs.clone();
    changed[5].1 = b"edited".to_vec();
    let s2 = snapshot(&a.store, &changed, &[s1], None);
    let st = push(&mut a, &srv, &kr, s2);
    // blob + d5 tree + work tree + root tree + meta tree + 1 meta blob (snapshot.json) + commit
    assert!(st.objects <= 7, "{st:?}");
    assert_eq!(srv.puts(), st.objects);
}

#[test]
fn known_set_prunes_without_server_call() {
    let kr = Keyring::generate();
    let srv = MemServer::default();
    let mut a = side();
    let s1 = snapshot(&a.store, &[("a", b"a".to_vec())], &[], None);
    push(&mut a, &srv, &kr, s1);
    assert!(
        new_objects(&a.store, &[s1], &a.known, &kr)
            .unwrap()
            .is_empty()
    );
    let reloaded = KnownSet::load(&a._d.path().join("known.bin")).unwrap();
    assert!(
        new_objects(&a.store, &[s1], &reloaded, &kr)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn large_blob_roundtrip_chunked() {
    let kr = Keyring::generate();
    let srv = MemServer::default();
    let mut a = side();
    let b = side();
    let mut big = vec![0u8; 20 << 20];
    rand::thread_rng().fill_bytes(&mut big);
    let s1 = snapshot(&a.store, &[("video.bin", big.clone())], &[], None);
    let st1 = push(&mut a, &srv, &kr, s1);
    assert!(st1.chunks >= 5, "{st1:?}");
    big[10 << 20] ^= 0xff;
    let s2 = snapshot(&a.store, &[("video.bin", big.clone())], &[s1], None);
    let st2 = push(&mut a, &srv, &kr, s2);
    assert!(st2.chunks <= 3, "only chunks around the edit: {st2:?}");
    fetch_closure(&b.store, &srv, &kr, &[s2]).unwrap();
    let tree = b.store.commit_tree(s2).unwrap();
    let work = b
        .store
        .read_tree(b.store.subtree(tree, "work").unwrap().unwrap())
        .unwrap();
    assert_eq!(
        b.store
            .read_blob(work[&RelPath::new("video.bin")].oid)
            .unwrap(),
        big
    );
}

#[test]
fn text_objects_are_compressed_incompressible_stored_raw() {
    let kr = Keyring::generate();
    let srv = MemServer::default();
    let mut a = side();
    let text = "spoko sync is fast\n".repeat(5000).into_bytes();
    let mut noise = vec![0u8; 100 << 10];
    rand::thread_rng().fill_bytes(&mut noise);
    let s = snapshot(
        &a.store,
        &[("t.txt", text.clone()), ("n.bin", noise.clone())],
        &[],
        None,
    );
    push(&mut a, &srv, &kr, s);
    let t_oid = Oid::hash_object(git2::ObjectType::Blob, &text).unwrap();
    let n_oid = Oid::hash_object(git2::ObjectType::Blob, &noise).unwrap();
    let objs = srv.objects.lock().unwrap();
    let t_sealed = &objs[&spoko_crypto::git_object_id(&kr, t_oid.as_bytes())];
    let n_sealed = &objs[&spoko_crypto::git_object_id(&kr, n_oid.as_bytes())];
    assert!(t_sealed.len() < 30 << 10, "{} bytes", t_sealed.len());
    let plain = spoko_crypto::open(
        &kr,
        &spoko_crypto::git_object_id(&kr, n_oid.as_bytes()).0,
        n_sealed,
    )
    .unwrap();
    assert_eq!(plain[1], 0, "raw flag for incompressible data");
}

#[test]
fn integrity_error_on_tampered_object() {
    let kr = Keyring::generate();
    let srv = MemServer::default();
    let mut a = side();
    let b = side();
    let s = snapshot(&a.store, &[("a", b"a".to_vec())], &[], None);
    push(&mut a, &srv, &kr, s);
    for v in srv.objects.lock().unwrap().values_mut() {
        let last = v.len() - 1;
        v[last] ^= 1;
    }
    assert!(fetch_closure(&b.store, &srv, &kr, &[s]).is_err());
}

#[test]
fn server_only_sees_opaque_ids() {
    let kr = Keyring::generate();
    let srv = MemServer::default();
    let mut a = side();
    let s = snapshot(
        &a.store,
        &[("secret.txt", b"password=hunter2".to_vec())],
        &[],
        None,
    );
    push(&mut a, &srv, &kr, s);
    let objs = srv.objects.lock().unwrap();
    let mut all_oids = vec![s.to_string()];
    let w = a.store.read_tree(a.store.commit_tree(s).unwrap()).unwrap();
    all_oids.extend(w.values().map(|e| e.oid.to_string()));
    for (id, bytes) in objs.iter() {
        assert!(
            !all_oids.contains(&id.to_string()[..40].to_string()),
            "raw oid used as id"
        );
        assert!(
            !bytes.windows(10).any(|w| w == b"secret.txt"),
            "plaintext name leaked"
        );
        assert!(
            !bytes
                .windows(8)
                .any(|w| w == b"hunter2!" || w == b"password"),
            "plaintext leaked"
        );
    }
}

fn user_history(store: &Store, n: usize) -> Oid {
    let mut parent: Option<Oid> = None;
    let empty = store.build_tree(&WorkMap::default()).unwrap();
    for i in 0..n {
        let mut w = WorkMap::default();
        w.insert(
            RelPath::new("f"),
            Entry {
                oid: store.write_blob(i.to_string().as_bytes()).unwrap(),
                mode: EntryMode::Blob,
            },
        );
        let t = store.build_tree(&w).unwrap();
        let _ = empty;
        parent = Some(
            store
                .commit(
                    t,
                    &parent.into_iter().collect::<Vec<_>>(),
                    &format!("c{i}"),
                    i as i64,
                )
                .unwrap(),
        );
    }
    parent.unwrap()
}

#[test]
fn gitlinks_bring_user_history_but_snapshot_parents_stay_behind() {
    let kr = Keyring::generate();
    let srv = MemServer::default();
    let mut a = side();
    let b = side();
    let tip = user_history(&a.store, 10);
    let mut gw = WorkMap::default();
    gw.insert(
        RelPath::new("tips/0"),
        Entry {
            oid: tip,
            mode: EntryMode::Commit,
        },
    );
    let gitstate = a.store.build_tree(&gw).unwrap();
    let s1 = snapshot(&a.store, &[("a", b"1".to_vec())], &[], None);
    push(&mut a, &srv, &kr, s1);
    let s2 = snapshot(&a.store, &[("a", b"2".to_vec())], &[s1], Some(gitstate));
    push(&mut a, &srv, &kr, s2);
    fetch_closure(&b.store, &srv, &kr, &[s2]).unwrap();
    let mut walk = b.store.repo().revwalk().unwrap();
    walk.push(tip).unwrap();
    assert_eq!(walk.count(), 10, "full user history arrived");
    assert!(!b.store.has(s1), "older snapshot not downloaded");
}

#[test]
fn interrupted_download_converges_on_retry() {
    let kr = Keyring::generate();
    let srv = MemServer::default();
    let mut a = side();
    let b = side();
    let files: Vec<(String, Vec<u8>)> = (0..300)
        .map(|i| (format!("d{}/f{i}", i % 30), format!("{i}").into_bytes()))
        .collect();
    let refs: Vec<(&str, Vec<u8>)> = files.iter().map(|(p, c)| (p.as_str(), c.clone())).collect();
    let s = snapshot(&a.store, &refs, &[], None);
    push(&mut a, &srv, &kr, s);
    *srv.fail_get_after.lock().unwrap() = Some(40);
    assert!(fetch_closure(&b.store, &srv, &kr, &[s]).is_err());
    let root = b.store.commit_tree(s);
    assert!(
        root.is_err() || !b.store.has(s),
        "no commit/tree written before its closure"
    );
    *srv.fail_get_after.lock().unwrap() = None;
    fetch_closure(&b.store, &srv, &kr, &[s]).unwrap();
    let work = b
        .store
        .read_tree(
            b.store
                .subtree(b.store.commit_tree(s).unwrap(), "work")
                .unwrap()
                .unwrap(),
        )
        .unwrap();
    assert_eq!(work.len(), 300);
}

#[test]
fn missing_required_object_is_integrity_error() {
    let kr = Keyring::generate();
    let srv = MemServer::default();
    let b = side();
    let a = side();
    let s = snapshot(&a.store, &[("a", b"a".to_vec())], &[], None);
    assert!(fetch_closure(&b.store, &srv, &kr, &[s]).is_err());
}
