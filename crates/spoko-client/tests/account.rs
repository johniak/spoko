use std::path::Path;
use std::time::Duration;

use spoko_client::account::{self, IDENTITY_FILE, Identity};
use spoko_client::api::ApiClient;
use spoko_client::ws::EventStream;
use spoko_crypto::{Keyring, RecoveryKey, test_util};
use spoko_proto::ServerEvent;
use spoko_server::test_util::{TestServer, spawn_test_server};

/// Logs a new machine in with test key `key` (the account is created for the key once).
fn login_in(srv: &TestServer, home: &Path, key: &str, name: &str, rk: Option<&RecoveryKey>) -> spoko_client::Result<(Identity, Option<RecoveryKey>)> {
    srv.create_account("jan", &test_util::key(key));
    account::login(home, &srv.url, &test_util::key_path(key), name, rk, None)
}

fn login(srv: &TestServer, name: &str) -> (Identity, Option<RecoveryKey>) {
    login_in(srv, tempfile::tempdir().unwrap().keep().as_path(), "id_ed25519", name, None).unwrap()
}

#[test]
fn first_login_creates_keyring_and_recovery_key() {
    let srv = spawn_test_server();
    let home = tempfile::tempdir().unwrap();
    let (id, rk) = login_in(&srv, home.path(), "id_ed25519", "mac", None).unwrap();
    assert!(rk.is_some());
    assert_eq!(id.device_name, "mac");
    assert_eq!(id.fingerprint, test_util::key("id_ed25519").fingerprint());
    let loaded = Identity::load(home.path(), None).unwrap();
    assert_eq!((loaded.device_id, &loaded.keyring), (id.device_id, &id.keyring));
    assert_eq!(id.api().devices().unwrap().len(), 1);
    assert!(Identity::load(tempfile::tempdir().unwrap().path(), None).is_err());
    assert_eq!(Identity::key_path(home.path()).unwrap(), test_util::key_path("id_ed25519"));
}

#[test]
fn second_machine_same_key_reads_same_data() {
    let srv = spawn_test_server();
    let (a, rk) = login(&srv, "mac");
    let (b, none) = login(&srv, "linux");
    assert!(rk.is_some() && none.is_none(), "one recovery key per account");
    assert_eq!(b.keyring, a.keyring);
    assert_ne!(b.device_id, a.device_id);
    assert_eq!(a.api().devices().unwrap().len(), 2);
    // a key of another account opens other data
    srv.create_account("other", &test_util::key("other_ed25519"));
    let (c, _) = account::login(tempfile::tempdir().unwrap().path(), &srv.url, &test_util::key_path("other_ed25519"), "x", None, None).unwrap();
    assert_ne!(c.keyring, a.keyring);
}

#[test]
fn identity_file_holds_no_secret() {
    let srv = spawn_test_server();
    let home = tempfile::tempdir().unwrap();
    let (id, _) = login_in(&srv, home.path(), "id_ed25519", "mac", None).unwrap();
    let bytes = std::fs::read(home.path().join(IDENTITY_FILE)).unwrap();
    let secret = id.keyring.to_bytes();
    assert!(!bytes.windows(32).any(|w| secret.windows(32).any(|s| s == w)), "keyring bytes on disk");
    // without the SSH key the identity does not open
    let moved = tempfile::tempdir().unwrap();
    let key = moved.path().join("id");
    std::fs::copy(test_util::key_path("id_ed25519"), &key).unwrap();
    let home2 = tempfile::tempdir().unwrap();
    account::login(home2.path(), &srv.url, &key, "mac", None, None).unwrap();
    std::fs::remove_file(&key).unwrap();
    assert!(matches!(Identity::load(home2.path(), None), Err(spoko_client::ClientError::Crypto(spoko_crypto::CryptoError::Io(_)))));
    // a different key in its place is refused
    std::fs::copy(test_util::key_path("other_ed25519"), &key).unwrap();
    let err = Identity::load(home2.path(), None).unwrap_err().to_string();
    assert!(err.contains("is not the SSH key this device logged in with"), "{err}");
}

#[test]
fn logging_in_again_keeps_the_device_id() {
    let srv = spawn_test_server();
    let home = tempfile::tempdir().unwrap();
    let (a, _) = login_in(&srv, home.path(), "id_ed25519", "mac", None).unwrap();
    let (again, rk) = login_in(&srv, home.path(), "id_ed25519", "mac", None).unwrap();
    assert_eq!((again.device_id, rk), (a.device_id, None));
    assert!(matches!(a.api().devices(), Err(spoko_client::ClientError::Unauthorized)), "old session ended");
    assert_eq!(again.api().devices().unwrap().len(), 1);
}

#[test]
fn passphrase_key_logs_in_with_the_passphrase() {
    let srv = spawn_test_server();
    let key = SshKeyPass::load();
    srv.create_account("p", &key);
    let home = tempfile::tempdir().unwrap();
    let path = test_util::key_path("id_ed25519_pass");
    assert!(matches!(
        account::login(home.path(), &srv.url, &path, "mac", None, None),
        Err(spoko_client::ClientError::Crypto(spoko_crypto::CryptoError::Encrypted))
    ));
    let (id, _) = account::login(home.path(), &srv.url, &path, "mac", None, Some("sekret")).unwrap();
    assert!(matches!(Identity::load(home.path(), None), Err(spoko_client::ClientError::Crypto(spoko_crypto::CryptoError::Encrypted))));
    assert_eq!(Identity::load(home.path(), Some("sekret")).unwrap().keyring, id.keyring);
}

struct SshKeyPass;
impl SshKeyPass {
    fn load() -> spoko_crypto::SshKey {
        spoko_crypto::SshKey::load(&test_util::key_path("id_ed25519_pass"), Some("sekret")).unwrap()
    }
}

#[test]
fn recovery_after_admin_set_key_reads_old_data() {
    let srv = spawn_test_server();
    let (a, rk) = login(&srv, "mac");
    let rk = rk.unwrap();
    let other = test_util::key("other_ed25519");
    let acc = srv.state.db.account_by_key(&test_util::key("id_ed25519").public_openssh()).unwrap().unwrap();
    srv.state.db.set_account_key(acc, &other.public_openssh(), &other.fingerprint()).unwrap();
    let new_key = test_util::key_path("other_ed25519");
    let err = account::login(tempfile::tempdir().unwrap().path(), &srv.url, &new_key, "mac", None, None).unwrap_err().to_string();
    assert!(err.contains("--recovery-key"), "{err}");
    assert!(account::login(tempfile::tempdir().unwrap().path(), &srv.url, &new_key, "mac", Some(&RecoveryKey::generate()), None).is_err(), "wrong recovery key");
    let (b, none) = account::login(tempfile::tempdir().unwrap().path(), &srv.url, &new_key, "mac", Some(&rk), None).unwrap();
    assert_eq!((b.keyring, none), (a.keyring.clone(), None));
    // from now on the new key alone is enough, the old one no longer logs in
    let (c, _) = account::login(tempfile::tempdir().unwrap().path(), &srv.url, &new_key, "linux", None, None).unwrap();
    assert_eq!(c.keyring, a.keyring);
    let old = account::login(tempfile::tempdir().unwrap().path(), &srv.url, &test_util::key_path("id_ed25519"), "old", None, None);
    assert!(matches!(old, Err(spoko_client::ClientError::Unauthorized)), "{old:?}");
}

#[test]
fn replaced_key_without_a_recovery_copy_is_an_error() {
    let srv = spawn_test_server();
    let (_, _) = login(&srv, "mac");
    srv.state.db.execute_raw("UPDATE accounts SET recovery_wrapped = NULL, wrapped_for = 'SHA256:old'").unwrap();
    let err = account::login(tempfile::tempdir().unwrap().path(), &srv.url, &test_util::key_path("id_ed25519"), "x", Some(&RecoveryKey::generate()), None).unwrap_err();
    assert!(err.to_string().contains("no recovery key"), "{err}");
}

#[test]
fn simultaneous_first_logins_share_one_keyring() {
    let srv = spawn_test_server();
    let key = test_util::key("id_ed25519");
    srv.create_account("jan", &key);
    // another machine stores its keyring between this login and this machine's store
    let theirs = Keyring::generate();
    let hex: String = key.wrap_keyring(&theirs).iter().map(|b| format!("{b:02x}")).collect();
    srv.state
        .db
        .execute_raw(&format!(
            "CREATE TRIGGER other_machine AFTER UPDATE OF last_seen ON devices BEGIN
               UPDATE accounts SET ssh_wrapped = x'{hex}', wrapped_for = ssh_fingerprint WHERE ssh_wrapped IS NULL;
             END;"
        ))
        .unwrap();
    let (id, rk) = account::login(tempfile::tempdir().unwrap().path(), &srv.url, &test_util::key_path("id_ed25519"), "mac", None, None).unwrap();
    assert_eq!((id.keyring, rk.is_none()), (theirs, true));
}

#[test]
fn removing_a_device_ends_its_session() {
    let srv = spawn_test_server();
    let (a, _) = login(&srv, "mac");
    let (b, _) = login(&srv, "linux");
    account::remove_device(&a, b.device_id).unwrap();
    assert!(matches!(b.api().devices(), Err(spoko_client::ClientError::Unauthorized)));
    assert!(account::remove_device(&a, uuid::Uuid::new_v4()).is_err());
}

#[test]
fn ws_receives_pointer_change_and_api_roundtrips() {
    let srv = spawn_test_server();
    let (a, _) = login(&srv, "a");
    let (b, _) = login(&srv, "b");
    let api_a = a.api();
    let p = api_a.create_project(b"meta").unwrap();
    assert_eq!(api_a.projects().unwrap(), vec![p.clone()]);
    let mut ws = EventStream::connect(&srv.url, &b.token).unwrap();
    assert!(matches!(
        ws.next(Duration::from_secs(5)).unwrap(),
        Some(ServerEvent::PresenceChanged { .. })
    ));
    assert_eq!(api_a.get_pointer(p.id, "head").unwrap(), None);
    let v1 = api_a
        .put_pointer(p.id, "head", Some(0), b"x")
        .unwrap()
        .unwrap();
    assert_eq!(v1.version, 1);
    assert_eq!(
        api_a
            .put_pointer(p.id, "head", Some(0), b"y")
            .unwrap()
            .unwrap_err()
            .version,
        1
    );
    assert_eq!(
        api_a.get_pointer(p.id, "head").unwrap().unwrap().value,
        b"x"
    );
    loop {
        match ws.next(Duration::from_secs(5)).unwrap() {
            Some(ServerEvent::PointerChanged { version, .. }) => {
                assert_eq!(version, 1);
                break;
            }
            Some(_) => continue,
            None => panic!("no event"),
        }
    }
    assert_eq!(ws.next(Duration::from_millis(50)).unwrap(), None);
    api_a.post_message(p.id, b"hello").unwrap();
    assert!(matches!(
        ws.next(Duration::from_secs(5)).unwrap(),
        Some(ServerEvent::Message { .. })
    ));
    assert_eq!(
        api_a.presence(p.id).unwrap().online_devices,
        vec![b.device_id]
    );
    let bad = ApiClient::new(&srv.url, Some("bogus".into()));
    assert!(matches!(
        bad.projects(),
        Err(spoko_client::ClientError::Unauthorized)
    ));
    assert!(EventStream::connect(&srv.url, "bogus").is_err());
    assert!(
        ApiClient::new("http://127.0.0.1:1", None)
            .projects()
            .is_err()
    );
}

#[test]
fn project_objects_implement_object_server() {
    use spoko_client::transfer::ObjectServer;
    let srv = spawn_test_server();
    let (a, _) = login(&srv, "a");
    let p = a.api().create_project(b"m").unwrap();
    let objs = a.api().objects(p.id);
    let id = spoko_crypto::ObjectId([5; 32]);
    assert_eq!(objs.missing(&[id]).unwrap(), vec![id]);
    objs.put(&[(id, b"sealed".to_vec())]).unwrap();
    assert_eq!(objs.get(&[id]).unwrap(), vec![(id, b"sealed".to_vec())]);
}
