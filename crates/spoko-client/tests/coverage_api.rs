//! Failure paths of the HTTP client, the event stream and the account operations.
#![cfg_attr(windows, allow(unused_imports, dead_code))] // helpers of #[cfg(unix)] tests

use std::net::TcpListener;
use std::time::Duration;

use spoko_client::ClientError;
use spoko_client::account::{self, Identity};
use spoko_client::api::ApiClient;
use spoko_client::transfer::ObjectServer;
use spoko_client::ws::EventStream;
use spoko_crypto::{ObjectId, RecoveryKey, test_util};
use spoko_proto::AccountKeyring;
use spoko_server::test_util::{TestServer, spawn_test_server};
use tungstenite::Message;
use uuid::Uuid;

fn first_device(srv: &TestServer, account: &str) -> (tempfile::TempDir, Identity) {
    let home = tempfile::tempdir().unwrap();
    srv.create_account(account, &test_util::key("id_ed25519"));
    let (id, _) = account::login(home.path(), &srv.url, &test_util::key_path("id_ed25519"), "a", None, None).unwrap();
    (home, id)
}

fn is_unauthorized<T>(r: spoko_client::Result<T>) -> bool {
    matches!(r, Err(ClientError::Unauthorized))
}

fn server_error<T: std::fmt::Debug>(r: spoko_client::Result<T>) -> String {
    match r {
        Err(ClientError::Server(m)) => m,
        other => panic!("expected a server error, got {other:?}"),
    }
}

#[test]
fn with_token_authenticates_an_anonymous_client() {
    let srv = spawn_test_server();
    let (_h, id) = first_device(&srv, "jan");
    let anon = ApiClient::new(&srv.url, None);
    assert!(is_unauthorized(anon.devices()));
    let devices = anon.with_token(&id.token).devices().unwrap();
    assert_eq!(
        devices.iter().map(|d| d.id).collect::<Vec<_>>(),
        vec![id.device_id]
    );
    assert!(
        is_unauthorized(anon.devices()),
        "the original client is unchanged"
    );
}

#[test]
fn oversized_object_is_rejected_by_the_server() {
    let srv = spawn_test_server();
    let (_h, id) = first_device(&srv, "jan");
    let p = id.api().create_project(b"m").unwrap();
    let big = vec![0; spoko_proto::MAX_OBJECT_SIZE + 1];
    let m = server_error(id.api().objects(p.id).put(&[(ObjectId([1; 32]), big)]));
    assert!(m.starts_with("HTTP 413"), "{m}");
}

#[test]
fn revoking_an_unknown_device_is_a_server_error() {
    let srv = spawn_test_server();
    let (_h, id) = first_device(&srv, "jan");
    assert_eq!(server_error(id.api().revoke(Uuid::new_v4())), "HTTP 404: ");
    assert_eq!(id.api().devices().unwrap().len(), 1);
}

#[test]
fn invalid_token_is_unauthorized_on_every_endpoint() {
    let srv = spawn_test_server();
    let (_h, id) = first_device(&srv, "jan");
    let p = id.api().create_project(b"m").unwrap().id;
    let bad = ApiClient::new(&srv.url, Some("bogus".into()));
    assert!(is_unauthorized(bad.revoke(id.device_id)));
    assert!(is_unauthorized(bad.keyring()));
    assert!(is_unauthorized(bad.create_project(b"m")));
    assert!(is_unauthorized(bad.get_pointer(p, "head")));
    assert!(is_unauthorized(bad.put_pointer(p, "head", None, b"v")));
    assert!(is_unauthorized(bad.post_message(p, b"hello")));
    assert!(is_unauthorized(bad.presence(p)));
    let kr = AccountKeyring { ssh_wrapped: vec![1], wrapped_for: String::new(), recovery_wrapped: None };
    assert!(is_unauthorized(bad.store_keyring(&kr)));
    let objs = bad.objects(p);
    let oid = ObjectId([2; 32]);
    assert!(is_unauthorized(objs.missing(&[oid])));
    assert!(is_unauthorized(objs.put(&[(oid, b"x".to_vec())])));
    assert!(is_unauthorized(objs.get(&[oid])));
    // nothing changed on the server
    assert_eq!(
        id.api()
            .devices()
            .unwrap()
            .iter()
            .filter(|d| d.revoked)
            .count(),
        0
    );
    assert_eq!(id.api().projects().unwrap().len(), 1);
    assert_eq!(id.api().get_pointer(p, "head").unwrap(), None);
}

#[test]
fn login_with_an_unregistered_key_writes_no_identity() {
    let srv = spawn_test_server();
    let home = tempfile::tempdir().unwrap();
    let r = account::login(home.path(), &srv.url, &test_util::key_path("id_ed25519"), "a", None, None);
    assert!(is_unauthorized(r));
    assert!(Identity::load(home.path(), None).is_err(), "no identity written");
}

#[test]
fn unreachable_server_is_a_network_error() {
    let r = account::login(tempfile::tempdir().unwrap().path(), "http://127.0.0.1:1", &test_util::key_path("id_ed25519"), "new", None, None);
    assert!(matches!(r, Err(ClientError::Network(_))));
}

#[test]
fn bad_challenge_answers_are_server_errors() {
    let srv = spawn_test_server();
    let anon = ApiClient::new(&srv.url, None);
    assert_eq!(server_error(anon.challenge("not a key")), "HTTP 400: ");
    let req = spoko_proto::LoginRequest { challenge_id: Uuid::new_v4(), signature: "x".into(), device_name: "d".into(), device_id: None };
    assert!(is_unauthorized(anon.login(&req)));
}

#[test]
fn keyring_missing_after_conflict_is_a_server_error() {
    let srv = spawn_test_server();
    srv.create_account("jan", &test_util::key("id_ed25519"));
    // a stored keyring the client is not allowed to replace, which then reads as missing
    srv.state.db.execute_raw("CREATE TRIGGER no_store BEFORE UPDATE OF ssh_wrapped ON accounts BEGIN SELECT RAISE(IGNORE); END;").unwrap();
    let r = account::login(tempfile::tempdir().unwrap().path(), &srv.url, &test_util::key_path("id_ed25519"), "a", None, None);
    assert!(matches!(r, Err(ClientError::Server(m)) if m == "keyring missing"));
}

#[test]
fn identity_debug_output_omits_secrets() {
    let srv = spawn_test_server();
    let (_h, id) = first_device(&srv, "jan");
    let s = format!("{id:?}");
    assert!(s.starts_with("Identity {"), "{s}");
    assert!(s.contains(&id.device_id.to_string()) && s.contains("\"a\"") && s.contains(&srv.url));
    assert!(s.ends_with(".. }"), "{s}");
    assert!(!s.contains(&id.token), "{s}");
}

#[test]
fn login_fails_when_identity_cannot_be_saved() {
    let srv = spawn_test_server();
    srv.create_account("jan", &test_util::key("id_ed25519"));
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("not-a-dir");
    std::fs::write(&home, b"x").unwrap();
    let r = account::login(&home, &srv.url, &test_util::key_path("id_ed25519"), "a", None, None);
    assert!(matches!(r, Err(ClientError::Io(_))), "{:?}", r.map(|_| ()));
}

#[test]
fn recovery_login_needs_a_valid_keyring() {
    let srv = spawn_test_server();
    let rk = RecoveryKey::generate();
    srv.create_account("jan", &test_util::key("id_ed25519"));
    // replaced key, and a recovery copy the key opens but which holds no keyring
    let hex: String = rk.wrap(b"not a keyring").iter().map(|b| format!("{b:02x}")).collect();
    srv.state.db.execute_raw(&format!("UPDATE accounts SET recovery_wrapped = x'{hex}'")).unwrap();
    let home = tempfile::tempdir().unwrap();
    let r = account::login(home.path(), &srv.url, &test_util::key_path("id_ed25519"), "b", Some(&rk), None);
    assert!(matches!(r, Err(ClientError::Crypto(_))), "{:?}", r.map(|_| ()));
    assert!(Identity::load(home.path(), None).is_err());
}

/// A WebSocket server that sends `frames` to the first client, then waits for it to leave.
fn fake_event_server(frames: Vec<Message>) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let t = std::thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        let mut ws = tungstenite::accept(s).unwrap();
        for f in frames {
            ws.send(f).unwrap();
        }
        while ws.read().is_ok() {}
    });
    (url, t)
}

#[test]
fn event_stream_rejects_malformed_events_and_reports_close() {
    let frames = vec![
        Message::Binary(vec![1, 2].into()),
        Message::Text("not json".into()),
        Message::Close(None),
    ];
    let (url, server) = fake_event_server(frames);
    let mut es = EventStream::connect(&url, "token").unwrap();
    let wait = Duration::from_secs(5);
    assert!(
        matches!(es.next(wait), Err(ClientError::Server(_))),
        "binary frames are skipped"
    );
    let closed = es.next(wait);
    assert!(matches!(closed, Err(ClientError::Network(m)) if m == "event stream closed"));
    drop(es);
    server.join().unwrap();
}
