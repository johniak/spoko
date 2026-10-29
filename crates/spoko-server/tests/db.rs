use spoko_crypto::ObjectId;
use spoko_proto::{AccountKeyring, Pointer};
use spoko_server::blobstore::BlobStore;
use spoko_server::db::Db;
use uuid::Uuid;

fn db() -> Db {
    Db::open_in_memory().unwrap()
}

/// An account whose (fake — the db does not parse keys) key line is `key <name>`.
fn account(db: &Db, name: &str) -> Uuid {
    db.create_account(name, &format!("key {name}"), &format!("fp {name}")).unwrap()
}

#[test]
fn cas_pointer_create_then_conflict() {
    let db = db();
    let acc = account(&db, "jan");
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
fn accounts_by_name_and_key() {
    let db = db();
    let acc = account(&db, "jan");
    assert!(db.create_account("jan", "key other", "fp").is_err(), "name taken");
    assert!(db.create_account("other", "key jan", "fp").is_err(), "key owns an account already");
    assert_eq!(db.account_by_name("jan").unwrap(), Some(acc));
    assert_eq!(db.account_by_name("nobody").unwrap(), None);
    assert_eq!(db.account_by_key("key jan").unwrap(), Some(acc));
    assert_eq!(db.account_by_key("key nobody").unwrap(), None);
}

#[test]
fn challenges_are_single_use_and_expire() {
    let db = db();
    let c = db.create_challenge("key jan", 1000).unwrap();
    assert_eq!(c.nonce.len(), 32);
    assert_eq!(db.take_challenge(c.challenge_id, 1001).unwrap(), Some(("key jan".into(), c.nonce.clone())));
    assert_eq!(db.take_challenge(c.challenge_id, 1001).unwrap(), None);
    let late = db.create_challenge("key jan", 1000).unwrap();
    assert_eq!(db.take_challenge(late.challenge_id, 1000 + 120).unwrap(), None, "120 s to answer");
    // old challenges are dropped when new ones are issued
    let stale = db.create_challenge("key jan", 1000).unwrap();
    db.create_challenge("key jan", 5000).unwrap();
    assert_eq!(db.take_challenge(stale.challenge_id, 0).unwrap(), None);
    assert_ne!(db.create_challenge("k", 0).unwrap().nonce, db.create_challenge("k", 0).unwrap().nonce);
}

#[test]
fn device_login_and_revoke() {
    let db = db();
    let acc = account(&db, "jan");
    let creds = db.login_device(acc, "mac", None).unwrap();
    let row = db.device_by_token(&creds.token).unwrap().unwrap();
    assert_eq!((row.id, row.account_id, row.name.as_str()), (creds.device_id, acc, "mac"));
    db.touch_device(creds.device_id, 42).unwrap();
    let list = db.list_devices(acc).unwrap();
    assert_eq!((list.len(), list[0].name.as_str(), list[0].last_seen, list[0].revoked), (1, "mac", Some(42), false));
    assert!(db.revoke_device(acc, creds.device_id).unwrap());
    assert!(!db.revoke_device(Uuid::new_v4(), creds.device_id).unwrap());
    assert!(db.device_by_token(&creds.token).unwrap().is_none());
    assert!(db.list_devices(acc).unwrap()[0].revoked);
    // logging in again as that device brings it back with a new token
    let again = db.login_device(acc, "mac2", Some(creds.device_id)).unwrap();
    assert_eq!(again.device_id, creds.device_id);
    assert!(db.device_by_token(&creds.token).unwrap().is_none());
    assert_eq!(db.device_by_token(&again.token).unwrap().unwrap().name, "mac2");
    // an id of another account is not taken over
    let other = account(&db, "other");
    let foreign = db.login_device(other, "x", Some(creds.device_id)).unwrap();
    assert_ne!(foreign.device_id, creds.device_id);
    assert_eq!(db.list_devices(acc).unwrap().len(), 1);
}

#[test]
fn keyring_stored_once_per_key() {
    let db = db();
    let acc = account(&db, "jan");
    assert_eq!(db.account_keyring(acc).unwrap(), None);
    assert!(db.store_keyring(acc, b"w1", Some(b"r1")).unwrap());
    assert!(!db.store_keyring(acc, b"w2", Some(b"r2")).unwrap());
    let kr = AccountKeyring { ssh_wrapped: b"w1".to_vec(), wrapped_for: "fp jan".into(), recovery_wrapped: Some(b"r1".to_vec()) };
    assert_eq!(db.account_keyring(acc).unwrap(), Some(kr));
    let dev = db.login_device(acc, "mac", None).unwrap();
    db.set_account_key(acc, "key new", "fp new").unwrap();
    assert!(db.device_by_token(&dev.token).unwrap().is_none(), "new key logs devices out");
    assert_eq!(db.account_by_key("key jan").unwrap(), None);
    assert_eq!(db.account_by_key("key new").unwrap(), Some(acc));
    assert_eq!(db.account_keyring(acc).unwrap().unwrap().wrapped_for, "fp jan", "stale until rewrapped");
    assert!(db.store_keyring(acc, b"w3", None).unwrap());
    let kr = db.account_keyring(acc).unwrap().unwrap();
    assert_eq!((kr.ssh_wrapped, kr.wrapped_for, kr.recovery_wrapped), (b"w3".to_vec(), "fp new".into(), Some(b"r1".to_vec())));
}

#[test]
fn account_with_only_a_recovery_copy_reports_it() {
    let db = db();
    let acc = account(&db, "jan");
    db.execute_raw("UPDATE accounts SET recovery_wrapped = x'09'").unwrap();
    let kr = db.account_keyring(acc).unwrap().unwrap();
    assert_eq!((kr.ssh_wrapped, kr.wrapped_for, kr.recovery_wrapped), (vec![], String::new(), Some(vec![9])));
}

#[test]
fn projects_scoped_to_account() {
    let db = db();
    let a = account(&db, "a");
    let b = account(&db, "b");
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
    let acc = account(&Db::open(&path).unwrap(), "jan");
    assert_eq!(Db::open(&path).unwrap().account_by_name("jan").unwrap(), Some(acc));
}

#[test]
fn blobstore_put_get_exists_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::open(&dir.path().join("blobs")).unwrap();
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
    let acc = account(&db, "jan");
    let dev = db.login_device(acc, "a", None).unwrap();
    let p = db.create_project(acc, b"m").unwrap();
    db.execute_raw("DROP TABLE accounts; DROP TABLE challenges; DROP TABLE devices; DROP TABLE projects; DROP TABLE pointers;").unwrap();
    let id = Uuid::new_v4();
    assert!(db.create_account("b", "k", "f").is_err());
    assert!(db.account_by_name("b").is_err());
    assert!(db.account_by_key("k").is_err());
    assert!(db.set_account_key(acc, "k", "f").is_err());
    assert!(db.create_challenge("k", 0).is_err());
    assert!(db.take_challenge(id, 0).is_err());
    assert!(db.login_device(acc, "a", None).is_err());
    assert!(db.login_device(acc, "a", Some(dev.device_id)).is_err());
    assert!(db.account_keyring(acc).is_err());
    assert!(db.store_keyring(acc, b"w", None).is_err());
    assert!(db.device_by_token(&dev.token).is_err());
    assert!(db.touch_device(id, 1).is_err());
    assert!(db.list_devices(acc).is_err());
    assert!(db.revoke_device(acc, id).is_err());
    assert!(db.create_project(acc, b"m").is_err());
    assert!(db.list_projects(acc).is_err());
    assert!(db.project_account(p).is_err());
    assert!(db.get_pointer(p, "h").is_err());
    assert!(db.cas_pointer(p, "h", None, b"v").is_err());
}

#[test]
fn partial_failures_surface() {
    let db = db();
    let acc = account(&db, "jan");
    db.execute_raw("CREATE TRIGGER no_revoke BEFORE UPDATE ON devices BEGIN SELECT RAISE(FAIL, 'ro'); END;").unwrap();
    db.login_device(acc, "a", None).unwrap();
    assert!(db.set_account_key(acc, "k2", "f2").is_err(), "logging devices out failed");
    assert_eq!(db.account_by_key("key jan").unwrap(), Some(acc), "rolled back");
    db.execute_raw("CREATE TRIGGER no_ins BEFORE INSERT ON challenges BEGIN SELECT RAISE(FAIL, 'ro'); END;").unwrap();
    assert!(db.create_challenge("k", 0).is_err());
}

#[test]
fn cas_write_failure_surfaces() {
    let db = db();
    let acc = account(&db, "jan");
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
    let store = BlobStore::open(dir.path()).unwrap();
    let p = Uuid::new_v4();
    let id = ObjectId([0xcd; 32]);
    store.put(p, &id, b"x").unwrap();
    // the pack vanished under the index
    for e in std::fs::read_dir(dir.path().join(p.to_string())).unwrap() {
        std::fs::remove_file(e.unwrap().path()).unwrap();
    }
    assert!(store.get(p, &id).is_err());
    // the project directory cannot be created
    let p2 = Uuid::new_v4();
    std::fs::write(dir.path().join(p2.to_string()), b"file blocks dir").unwrap();
    assert!(store.put(p2, &ObjectId([1; 32]), b"x").is_err());
    // unusable root
    let file = dir.path().join("plain-file");
    std::fs::write(&file, b"").unwrap();
    assert!(BlobStore::open(&file).is_err());
}

#[test]
fn blobstore_batches_dedup_and_keep_order() {
    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::open(dir.path()).unwrap();
    let p = Uuid::new_v4();
    let items: Vec<(ObjectId, Vec<u8>)> = (0..1000u32).map(|i| (ObjectId([(i % 251) as u8; 32]).with_suffix(i), i.to_le_bytes().to_vec())).collect();
    assert_eq!(store.put_batch(p, &items).unwrap(), 1000);
    assert_eq!(store.put_batch(p, &items).unwrap(), 0, "second upload stores nothing");
    let packs = std::fs::read_dir(dir.path().join(p.to_string())).unwrap().count();
    assert_eq!(packs, 1, "one pack (one fsync) per batch");
    let want: Vec<ObjectId> = items.iter().rev().take(10).map(|(i, _)| *i).chain([ObjectId([0xff; 32])]).collect();
    let got = store.get_many(p, &want).unwrap();
    assert_eq!(got.len(), 10);
    assert_eq!(got[0], items[999]);
    assert_eq!(store.missing(p, &want).unwrap(), vec![ObjectId([0xff; 32])]);
    assert_eq!(store.put_batch(p, &[(ObjectId([7; 32]), b"a".to_vec()), (ObjectId([7; 32]), b"a".to_vec())]).unwrap(), 1, "duplicates inside a batch");
}

trait WithSuffix {
    fn with_suffix(self, i: u32) -> ObjectId;
}
impl WithSuffix for ObjectId {
    fn with_suffix(mut self, i: u32) -> ObjectId {
        self.0[28..].copy_from_slice(&i.to_le_bytes());
        self
    }
}
