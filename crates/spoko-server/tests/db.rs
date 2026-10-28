use spoko_crypto::ObjectId;
use spoko_proto::{PairingStatus, Pointer};
use spoko_server::blobstore::BlobStore;
use spoko_server::db::{Db, PollOutcome};
use uuid::Uuid;

fn db() -> Db {
    Db::open_in_memory().unwrap()
}

#[test]
fn cas_pointer_create_then_conflict() {
    let db = db();
    let acc = db.create_account("jan").unwrap();
    let p = db.create_project(acc, b"meta").unwrap();
    assert_eq!(db.get_pointer(p, "head").unwrap(), None);
    let v1 = db.cas_pointer(p, "head", Some(0), b"a").unwrap().unwrap();
    assert_eq!(v1, Pointer { version: 1, value: b"a".to_vec() });
    let conflict = db.cas_pointer(p, "head", Some(0), b"b").unwrap().unwrap_err();
    assert_eq!(conflict.version, 1);
    let v2 = db.cas_pointer(p, "head", Some(1), b"c").unwrap().unwrap();
    assert_eq!(v2.version, 2);
    let missing = db.cas_pointer(p, "other", Some(5), b"x").unwrap().unwrap_err();
    assert_eq!(missing, Pointer { version: 0, value: vec![] });
    let forced = db.cas_pointer(p, "head", None, b"d").unwrap().unwrap();
    assert_eq!(forced.version, 3);
    let created = db.cas_pointer(p, "fresh", None, b"e").unwrap().unwrap();
    assert_eq!(created.version, 1);
    assert_eq!(db.get_pointer(p, "head").unwrap().unwrap().value, b"d");
}

#[test]
fn bootstrap_token_single_use() {
    let db = db();
    let acc = db.create_account("jan").unwrap();
    assert!(db.create_account("jan").is_err());
    assert_eq!(db.account_by_name("jan").unwrap(), Some(acc));
    assert_eq!(db.account_by_name("nobody").unwrap(), None);
    let t = db.issue_bootstrap_token(acc).unwrap();
    assert_eq!(db.peek_bootstrap_token(&t).unwrap(), Some(acc));
    assert_eq!(db.consume_bootstrap_token(&t).unwrap(), Some(acc));
    assert_eq!(db.consume_bootstrap_token(&t).unwrap(), None);
    assert_eq!(db.peek_bootstrap_token(&t).unwrap(), None);
    assert_eq!(db.consume_bootstrap_token("bogus").unwrap(), None);
}

#[test]
fn device_by_token_ignores_revoked() {
    let db = db();
    let acc = db.create_account("jan").unwrap();
    let creds = db.create_device(acc, "mac", &[1; 32], b"wk").unwrap();
    let row = db.device_by_token(&creds.token).unwrap().unwrap();
    assert_eq!((row.id, row.account_id), (creds.device_id, acc));
    assert_eq!(db.device_keyring(creds.device_id).unwrap(), Some(b"wk".to_vec()));
    db.touch_device(creds.device_id, 42).unwrap();
    let list = db.list_devices(acc).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!((list[0].name.as_str(), list[0].last_seen, list[0].revoked), ("mac", Some(42), false));
    assert!(db.revoke_device(acc, creds.device_id).unwrap());
    assert!(!db.revoke_device(Uuid::new_v4(), creds.device_id).unwrap());
    assert!(db.device_by_token(&creds.token).unwrap().is_none());
    assert!(db.list_devices(acc).unwrap()[0].revoked);
}

#[test]
fn keyrings_and_recovery_update() {
    let db = db();
    let acc = db.create_account("jan").unwrap();
    let a = db.create_device(acc, "a", &[1; 32], b"old").unwrap();
    assert_eq!(db.get_recovery(acc).unwrap(), None);
    db.set_recovery(acc, b"r1").unwrap();
    assert_eq!(db.get_recovery(acc).unwrap(), Some(b"r1".to_vec()));
    db.update_keyrings(acc, &[(a.device_id, b"new".to_vec()), (Uuid::new_v4(), b"x".to_vec())], Some(b"r2")).unwrap();
    assert_eq!(db.device_keyring(a.device_id).unwrap(), Some(b"new".to_vec()));
    assert_eq!(db.get_recovery(acc).unwrap(), Some(b"r2".to_vec()));
    db.update_keyrings(acc, &[], None).unwrap();
    assert_eq!(db.get_recovery(acc).unwrap(), Some(b"r2".to_vec()));
}

#[test]
fn pairing_flow_and_expiry() {
    let db = db();
    let acc = db.create_account("jan").unwrap();
    let started = db.create_pairing("linux", &[2; 32], 1000).unwrap();
    assert_eq!(started.code.len(), 8);
    assert!(started.code.chars().all(|c| "ABCDEFGHJKLMNPQRSTUVWXYZ23456789".contains(c)));
    assert_eq!(db.poll_pairing(started.pairing_id, &started.secret, 1001).unwrap(), PollOutcome::Status(PairingStatus::Pending));
    assert_eq!(db.poll_pairing(started.pairing_id, "wrong", 1001).unwrap(), PollOutcome::NotFound);
    let info = db.pairing_by_code(&started.code.to_lowercase(), 1002).unwrap().unwrap();
    assert_eq!((info.device_name.as_str(), info.device_pubkey), ("linux", [2; 32]));
    let creds = db.approve_pairing(started.pairing_id, acc, b"wk", 1003).unwrap().unwrap();
    assert!(db.approve_pairing(started.pairing_id, acc, b"wk", 1003).unwrap().is_none());
    match db.poll_pairing(started.pairing_id, &started.secret, 1004).unwrap() {
        PollOutcome::Status(PairingStatus::Approved { credentials, wrapped_keyring }) => {
            assert_eq!(credentials, creds);
            assert_eq!(wrapped_keyring, b"wk");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(db.poll_pairing(started.pairing_id, &started.secret, 1005).unwrap(), PollOutcome::Gone);
    assert!(db.device_by_token(&creds.token).unwrap().is_some());

    let late = db.create_pairing("win", &[3; 32], 1000).unwrap();
    let expired_at = 1000 + 601;
    assert!(db.pairing_by_code(&late.code, expired_at).unwrap().is_none());
    assert!(db.approve_pairing(late.pairing_id, acc, b"wk", expired_at).unwrap().is_none());
    assert_eq!(db.poll_pairing(late.pairing_id, &late.secret, expired_at).unwrap(), PollOutcome::Status(PairingStatus::Expired));
    assert_eq!(db.poll_pairing(Uuid::new_v4(), "x", 0).unwrap(), PollOutcome::NotFound);
}

#[test]
fn projects_scoped_to_account() {
    let db = db();
    let a = db.create_account("a").unwrap();
    let b = db.create_account("b").unwrap();
    let p = db.create_project(a, b"m").unwrap();
    assert_eq!(db.project_account(p).unwrap(), Some(a));
    assert_eq!(db.project_account(Uuid::new_v4()).unwrap(), None);
    assert_eq!(db.list_projects(a).unwrap().len(), 1);
    assert_eq!(db.list_projects(a).unwrap()[0].encrypted_meta, b"m");
    assert!(db.list_projects(b).unwrap().is_empty());
}

#[test]
fn migrations_reopen_keeps_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spoko.db");
    let acc = Db::open(&path).unwrap().create_account("jan").unwrap();
    assert_eq!(Db::open(&path).unwrap().account_by_name("jan").unwrap(), Some(acc));
}

#[test]
fn blobstore_put_get_exists_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path().join("blobs"));
    let p = Uuid::new_v4();
    let id = ObjectId([0xab; 32]);
    assert!(!store.exists(p, &id));
    assert_eq!(store.get(p, &id).unwrap(), None);
    store.put(p, &id, b"data").unwrap();
    store.put(p, &id, b"data").unwrap();
    assert!(store.exists(p, &id));
    assert_eq!(store.get(p, &id).unwrap(), Some(b"data".to_vec()));
    assert!(!store.exists(Uuid::new_v4(), &id));
    let leftovers: Vec<_> = walk(dir.path()).into_iter().filter(|n| n.contains("tmp")).collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

fn walk(p: &std::path::Path) -> Vec<String> {
    let mut out = vec![];
    for e in std::fs::read_dir(p).unwrap() {
        let e = e.unwrap();
        if e.file_type().unwrap().is_dir() {
            out.extend(walk(&e.path()));
        } else {
            out.push(e.file_name().to_string_lossy().into_owned());
        }
    }
    out
}

#[test]
fn every_query_surfaces_sql_errors() {
    let db = db();
    let acc = db.create_account("jan").unwrap();
    let dev = db.create_device(acc, "a", &[1; 32], b"w").unwrap();
    let p = db.create_project(acc, b"m").unwrap();
    let pairing = db.create_pairing("x", &[2; 32], 0).unwrap();
    db.execute_raw("DROP TABLE accounts; DROP TABLE bootstrap_tokens; DROP TABLE devices; DROP TABLE pairings; DROP TABLE projects; DROP TABLE pointers;").unwrap();
    let id = Uuid::new_v4();
    assert!(db.create_account("b").is_err());
    assert!(db.account_by_name("b").is_err());
    assert!(db.issue_bootstrap_token(acc).is_err());
    assert!(db.peek_bootstrap_token("t").is_err());
    assert!(db.consume_bootstrap_token("t").is_err());
    assert!(db.set_recovery(acc, b"r").is_err());
    assert!(db.get_recovery(acc).is_err());
    assert!(db.create_device(acc, "a", &[1; 32], b"w").is_err());
    assert!(db.device_by_token(&dev.token).is_err());
    assert!(db.touch_device(id, 1).is_err());
    assert!(db.list_devices(acc).is_err());
    assert!(db.revoke_device(acc, id).is_err());
    assert!(db.device_keyring(id).is_err());
    assert!(db.update_keyrings(acc, &[(id, vec![])], None).is_err());
    assert!(db.update_keyrings(acc, &[], Some(b"r")).is_err());
    assert!(db.create_pairing("x", &[2; 32], 0).is_err());
    assert!(db.pairing_by_code("AAAA", 0).is_err());
    assert!(db.approve_pairing(pairing.pairing_id, acc, b"w", 0).is_err());
    assert!(db.poll_pairing(pairing.pairing_id, &pairing.secret, 0).is_err());
    assert!(db.create_project(acc, b"m").is_err());
    assert!(db.list_projects(acc).is_err());
    assert!(db.project_account(p).is_err());
    assert!(db.get_pointer(p, "h").is_err());
    assert!(db.cas_pointer(p, "h", None, b"v").is_err());
}

#[test]
fn approve_fails_cleanly_when_device_insert_fails() {
    let db = db();
    let acc = db.create_account("jan").unwrap();
    let pairing = db.create_pairing("x", &[2; 32], 0).unwrap();
    db.execute_raw("DROP TABLE devices;").unwrap();
    assert!(db.approve_pairing(pairing.pairing_id, acc, b"w", 1).is_err());
}

#[test]
fn poll_delivery_update_failure_surfaces() {
    let db = db();
    let acc = db.create_account("jan").unwrap();
    let pairing = db.create_pairing("x", &[2; 32], 0).unwrap();
    db.approve_pairing(pairing.pairing_id, acc, b"w", 1).unwrap().unwrap();
    db.execute_raw("CREATE TRIGGER no_update BEFORE UPDATE ON pairings BEGIN SELECT RAISE(FAIL, 'ro'); END;").unwrap();
    assert!(db.poll_pairing(pairing.pairing_id, &pairing.secret, 2).is_err());
}

#[test]
fn cas_write_failure_surfaces() {
    let db = db();
    let acc = db.create_account("jan").unwrap();
    let p = db.create_project(acc, b"m").unwrap();
    db.execute_raw("CREATE TRIGGER no_ins BEFORE INSERT ON pointers BEGIN SELECT RAISE(FAIL, 'ro'); END;").unwrap();
    assert!(db.cas_pointer(p, "h", None, b"v").is_err());
}

#[test]
fn open_fails_on_directory_path() {
    let dir = tempfile::tempdir().unwrap();
    assert!(Db::open(dir.path()).is_err());
}

#[test]
fn blobstore_surfaces_io_errors() {
    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path().to_path_buf());
    let p = Uuid::new_v4();
    let id = ObjectId([0xcd; 32]);
    let hex = id.to_string();
    std::fs::create_dir_all(dir.path().join(p.to_string()).join(&hex[..2]).join(&hex[2..])).unwrap();
    assert!(store.get(p, &id).is_err());
    let id2 = ObjectId([0xce; 32]);
    std::fs::write(dir.path().join(p.to_string()).join("ce"), b"file blocks dir").unwrap();
    assert!(store.put(p, &id2, b"x").is_err());
}
