use spoko_client::account::{self, Identity};
use spoko_client::api::ApiClient;
use spoko_client::ws::EventStream;
use spoko_crypto::RecoveryKey;
use spoko_proto::ServerEvent;
use spoko_server::test_util::spawn_test_server;
use std::time::Duration;

#[test]
fn bootstrap_first_device_returns_recovery_key() {
    let srv = spawn_test_server();
    let home = tempfile::tempdir().unwrap();
    let (id, rk) = account::login_bootstrap(home.path(), &srv.url, &srv.bootstrap_token("jan"), "mac", None).unwrap();
    assert!(rk.is_some());
    assert_eq!(id.device_name, "mac");
    let loaded = Identity::load(home.path()).unwrap();
    assert_eq!(loaded.device_id, id.device_id);
    assert_eq!(loaded.keyring, id.keyring);
    assert_eq!(loaded.keypair.public(), id.keypair.public());
    assert_eq!(id.api().devices().unwrap().len(), 1);
    assert!(Identity::load(tempfile::tempdir().unwrap().path()).is_err());
}

#[test]
fn recovery_login_restores_same_keyring() {
    let srv = spawn_test_server();
    let (a, rk) = account::login_bootstrap(tempfile::tempdir().unwrap().path(), &srv.url, &srv.bootstrap_token("jan"), "a", None).unwrap();
    let rk = rk.unwrap();
    let home = tempfile::tempdir().unwrap();
    let (b, none) = account::login_bootstrap(home.path(), &srv.url, &srv.bootstrap_token("jan"), "b", Some(&rk)).unwrap();
    assert!(none.is_none());
    assert_eq!(b.keyring, a.keyring);
    let wrong = RecoveryKey::generate();
    assert!(account::login_bootstrap(tempfile::tempdir().unwrap().path(), &srv.url, &srv.bootstrap_token("jan"), "c", Some(&wrong)).is_err());
}

#[test]
fn pairing_approve_transfers_keyring() {
    let srv = spawn_test_server();
    let (a, _) = account::login_bootstrap(tempfile::tempdir().unwrap().path(), &srv.url, &srv.bootstrap_token("jan"), "a", None).unwrap();
    let home_b = tempfile::tempdir().unwrap();
    let pending = account::pair_start(&srv.url, "linux").unwrap();
    let code = pending.code.clone();
    let fp = pending.fingerprint.clone();
    let srv_url = srv.url.clone();
    let _ = srv_url;
    let approver = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        account::pair_approve(&a, &code, |name, shown| name == "linux" && shown == fp).unwrap()
    });
    let b = pending.wait(home_b.path(), Duration::from_secs(10), Duration::from_millis(20)).unwrap();
    assert!(approver.join().unwrap());
    assert_eq!(Identity::load(home_b.path()).unwrap().keyring, b.keyring);
    assert_eq!(b.api().devices().unwrap().len(), 2);
}

#[test]
fn pairing_rejected_when_fingerprint_not_confirmed_and_wait_times_out() {
    let srv = spawn_test_server();
    let (a, _) = account::login_bootstrap(tempfile::tempdir().unwrap().path(), &srv.url, &srv.bootstrap_token("jan"), "a", None).unwrap();
    let pending = account::pair_start(&srv.url, "linux").unwrap();
    assert!(!account::pair_approve(&a, &pending.code, |_, _| false).unwrap());
    assert!(account::pair_approve(&a, "NOPE2345", |_, _| true).is_err());
    let err = pending.wait(tempfile::tempdir().unwrap().path(), Duration::from_millis(100), Duration::from_millis(20)).unwrap_err();
    assert!(err.to_string().contains("timed out"), "{err}");
}

#[test]
fn revoke_rotates_and_other_device_refreshes() {
    let srv = spawn_test_server();
    let home_a = tempfile::tempdir().unwrap();
    let (mut a, rk) = account::login_bootstrap(home_a.path(), &srv.url, &srv.bootstrap_token("jan"), "a", None).unwrap();
    let home_b = tempfile::tempdir().unwrap();
    let (mut b, _) = account::login_bootstrap(home_b.path(), &srv.url, &srv.bootstrap_token("jan"), "b", rk.as_ref()).unwrap();
    let (c, _) = account::login_bootstrap(tempfile::tempdir().unwrap().path(), &srv.url, &srv.bootstrap_token("jan"), "c", rk.as_ref()).unwrap();
    let new_rk = account::revoke_device(&mut a, home_a.path(), c.device_id).unwrap();
    assert_eq!(a.keyring.current().0, 2);
    assert!(matches!(c.api().devices(), Err(spoko_client::ClientError::Unauthorized)));
    account::refresh_keyring(&mut b, home_b.path()).unwrap();
    assert_eq!(b.keyring, a.keyring);
    // the new recovery key works, the old one does not
    let (d, _) = account::login_bootstrap(tempfile::tempdir().unwrap().path(), &srv.url, &srv.bootstrap_token("jan"), "d", Some(&new_rk)).unwrap();
    assert_eq!(d.keyring, a.keyring);
    assert!(account::login_bootstrap(tempfile::tempdir().unwrap().path(), &srv.url, &srv.bootstrap_token("jan"), "e", rk.as_ref()).is_err());
}

#[test]
fn ws_receives_pointer_change_and_api_roundtrips() {
    let srv = spawn_test_server();
    let (a, rk) = account::login_bootstrap(tempfile::tempdir().unwrap().path(), &srv.url, &srv.bootstrap_token("jan"), "a", None).unwrap();
    let (b, _) = account::login_bootstrap(tempfile::tempdir().unwrap().path(), &srv.url, &srv.bootstrap_token("jan"), "b", rk.as_ref()).unwrap();
    let api_a = a.api();
    let p = api_a.create_project(b"meta").unwrap();
    assert_eq!(api_a.projects().unwrap(), vec![p.clone()]);
    let mut ws = EventStream::connect(&srv.url, &b.token).unwrap();
    assert!(matches!(ws.next(Duration::from_secs(5)).unwrap(), Some(ServerEvent::PresenceChanged { .. })));
    assert_eq!(api_a.get_pointer(p.id, "head").unwrap(), None);
    let v1 = api_a.put_pointer(p.id, "head", Some(0), b"x").unwrap().unwrap();
    assert_eq!(v1.version, 1);
    assert_eq!(api_a.put_pointer(p.id, "head", Some(0), b"y").unwrap().unwrap_err().version, 1);
    assert_eq!(api_a.get_pointer(p.id, "head").unwrap().unwrap().value, b"x");
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
    assert!(matches!(ws.next(Duration::from_secs(5)).unwrap(), Some(ServerEvent::Message { .. })));
    assert_eq!(api_a.presence(p.id).unwrap().online_devices, vec![b.device_id]);
    let bad = ApiClient::new(&srv.url, Some("bogus".into()));
    assert!(matches!(bad.projects(), Err(spoko_client::ClientError::Unauthorized)));
    assert!(EventStream::connect(&srv.url, "bogus").is_err());
    assert!(ApiClient::new("http://127.0.0.1:1", None).projects().is_err());
}

#[test]
fn project_objects_implement_object_server() {
    use spoko_client::transfer::ObjectServer;
    let srv = spawn_test_server();
    let (a, _) = account::login_bootstrap(tempfile::tempdir().unwrap().path(), &srv.url, &srv.bootstrap_token("jan"), "a", None).unwrap();
    let p = a.api().create_project(b"m").unwrap();
    let objs = a.api().objects(p.id);
    let id = spoko_crypto::ObjectId([5; 32]);
    assert_eq!(objs.missing(&[id]).unwrap(), vec![id]);
    objs.put(&[(id, b"sealed".to_vec())]).unwrap();
    assert_eq!(objs.get(&[id]).unwrap(), vec![(id, b"sealed".to_vec())]);
}
