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
    let old_data = spoko_crypto::seal(&a.keyring, b"x", b"before");
    let (b, new_rk) = account::login(tempfile::tempdir().unwrap().path(), &srv.url, &new_key, "mac", Some(&rk), None).unwrap();
    // whoever has the lost key also has the old keyring: new data gets a new key version
    assert_eq!(b.keyring.current().0, a.keyring.current().0 + 1);
    assert_eq!(spoko_crypto::open(&b.keyring, b"x", &old_data).unwrap(), b"before", "old data stays readable");
    assert!(spoko_crypto::open(&a.keyring, b"x", &spoko_crypto::seal(&b.keyring, b"x", b"after")).is_err());
    // and a new recovery key replaces the old one on the server
    let new_rk = new_rk.expect("a new recovery key");
    let stored = srv.state.db.account_keyring(acc).unwrap().unwrap().recovery_wrapped.unwrap();
    assert_eq!(Keyring::from_bytes(&new_rk.unwrap(&stored).unwrap()).unwrap(), b.keyring);
    assert!(rk.unwrap(&stored).is_err(), "the old recovery key opens nothing any more");
    // from now on the new key alone is enough, the old one no longer logs in
    let (c, none) = account::login(tempfile::tempdir().unwrap().path(), &srv.url, &new_key, "linux", None, None).unwrap();
    assert_eq!((&c.keyring, none.is_none()), (&b.keyring, true));
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
fn simultaneous_recoveries_share_one_keyring() {
    let srv = spawn_test_server();
    let (_, rk) = login(&srv, "mac");
    let other = test_util::key("other_ed25519");
    let acc = srv.state.db.account_by_key(&test_util::key("id_ed25519").public_openssh()).unwrap().unwrap();
    srv.state.db.set_account_key(acc, &other.public_openssh(), &other.fingerprint()).unwrap();
    // another machine finishes its recovery between this login and this machine's store
    let theirs = Keyring::generate();
    let hex: String = other.wrap_keyring(&theirs).iter().map(|b| format!("{b:02x}")).collect();
    srv.state
        .db
        .execute_raw(&format!(
            "CREATE TRIGGER other_machine AFTER UPDATE OF last_seen ON devices BEGIN
               UPDATE accounts SET ssh_wrapped = x'{hex}', wrapped_for = ssh_fingerprint WHERE wrapped_for != ssh_fingerprint;
             END;"
        ))
        .unwrap();
    let (id, new_rk) = account::login(tempfile::tempdir().unwrap().path(), &srv.url, &test_util::key_path("other_ed25519"), "x", rk.as_ref(), None).unwrap();
    assert_eq!((id.keyring, new_rk.is_none()), (theirs, true), "theirs won; its recovery key is the one shown there");
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

#[test]
fn key_rotate_old_key_rejected_other_devices_relogin() {
    let srv = spawn_test_server();
    let home_a = tempfile::tempdir().unwrap();
    let (mut a, old_rk) = login_in(&srv, home_a.path(), "id_ed25519", "mac", None).unwrap();
    let (b, _) = login(&srv, "linux");
    let before = a.keyring.clone();
    let new_rk = account::rotate_key(&mut a, home_a.path(), None, &test_util::key_path("id_ed25519_pass"), Some("sekret")).unwrap();
    assert_eq!(a.keyring.current().0, before.current().0 + 1, "new data gets a new key version");
    assert_eq!(a.fingerprint, SshKeyPass::load().fingerprint());
    assert_eq!(Identity::load(home_a.path(), Some("sekret")).unwrap().keyring, a.keyring, "saved");
    assert!(matches!(b.api().devices(), Err(spoko_client::ClientError::Unauthorized)));
    // the other machine logs in with the new key and gets the rotated keyring
    let (b2, _) = account::login(tempfile::tempdir().unwrap().path(), &srv.url, &test_util::key_path("id_ed25519_pass"), "linux", None, Some("sekret")).unwrap();
    assert_eq!(b2.keyring, a.keyring);
    let old = account::login(tempfile::tempdir().unwrap().path(), &srv.url, &test_util::key_path("id_ed25519"), "x", None, None);
    assert!(matches!(old, Err(spoko_client::ClientError::Unauthorized)));
    // the server keeps only the new recovery copy: the old recovery key opens nothing any more
    let kr = srv.state.db.account_keyring(srv.state.db.account_by_key(&SshKeyPass::load().public_openssh()).unwrap().unwrap()).unwrap().unwrap();
    let rec = kr.recovery_wrapped.unwrap();
    assert_eq!(Keyring::from_bytes(&new_rk.unwrap(&rec).unwrap()).unwrap(), a.keyring);
    assert!(old_rk.unwrap().unwrap(&rec).is_err());
    // rotating to a key of another account fails and changes nothing
    srv.create_account("other", &test_util::key("other_ed25519"));
    assert!(account::rotate_key(&mut a, home_a.path(), Some("sekret"), &test_util::key_path("other_ed25519"), None).is_err());
    assert_eq!(a.fingerprint, SshKeyPass::load().fingerprint());
    // the current key has a passphrase now: rotating away from it needs it
    let ecdsa = test_util::key_path("id_ecdsa");
    assert!(matches!(account::rotate_key(&mut a, home_a.path(), None, &ecdsa, None), Err(spoko_client::ClientError::Crypto(spoko_crypto::CryptoError::Encrypted))));
    account::rotate_key(&mut a, home_a.path(), Some("sekret"), &ecdsa, None).unwrap();
    assert_eq!(a.fingerprint, test_util::key("id_ecdsa").fingerprint());
}

#[test]
fn a_session_token_alone_cannot_rotate_the_key() {
    use spoko_proto::{RotateKey, SignPurpose, signed_message};
    let srv = spawn_test_server();
    let home = tempfile::tempdir().unwrap();
    let (a, _) = login_in(&srv, home.path(), "id_ed25519", "mac", None).unwrap();
    // a thief with this device's token and its own key
    let api = a.api();
    let thief = test_util::key("id_ecdsa");
    let sign = |k: &spoko_crypto::SshKey, p, nonce: &[u8]| k.sign(&signed_message(p, &srv.url, nonce)).unwrap();
    let (oc, c) = (api.challenge(&test_util::key("id_ed25519").public_openssh()).unwrap(), api.challenge(&thief.public_openssh()).unwrap());
    let mut req = RotateKey {
        old_challenge_id: oc.challenge_id,
        old_signature: String::new(),
        challenge_id: c.challenge_id,
        signature: sign(&thief, SignPurpose::RotateNew, &c.nonce),
        ssh_wrapped: vec![1],
        recovery_wrapped: vec![2],
    };
    assert!(matches!(api.rotate_key(&req), Err(spoko_client::ClientError::Unauthorized)), "without the old key's signature");
    let (oc, c) = (api.challenge(&test_util::key("id_ed25519").public_openssh()).unwrap(), api.challenge(&thief.public_openssh()).unwrap());
    (req.old_challenge_id, req.old_signature) = (oc.challenge_id, sign(&thief, SignPurpose::RotateOld, &oc.nonce));
    (req.challenge_id, req.signature) = (c.challenge_id, sign(&thief, SignPurpose::RotateNew, &c.nonce));
    assert!(matches!(api.rotate_key(&req), Err(spoko_client::ClientError::Unauthorized)), "old signature by the wrong key");
    // the account, its keyring and this device are as before
    let id = Identity::load(home.path(), None).unwrap();
    assert_eq!((id.fingerprint, id.keyring), (a.fingerprint.clone(), a.keyring.clone()));
    let kr = api.keyring().unwrap().unwrap();
    assert_eq!(test_util::key("id_ed25519").unwrap_keyring(&kr.ssh_wrapped).unwrap(), a.keyring);
    assert!(kr.recovery_wrapped.is_some_and(|r| r != vec![2]));
    assert_eq!(api.devices().unwrap().len(), 1);
}
