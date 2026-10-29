//! Failure paths of the HTTP client, the event stream and the account operations.
#![cfg_attr(windows, allow(unused_imports, dead_code))] // helpers of #[cfg(unix)] tests

use std::net::TcpListener;
use std::time::Duration;

use spoko_client::ClientError;
use spoko_client::account::{self, Identity};
use spoko_client::api::{ApiClient, PollResult};
use spoko_client::transfer::ObjectServer;
use spoko_client::ws::EventStream;
use spoko_crypto::{DeviceKeypair, ObjectId, RecoveryKey};
use spoko_proto::{KeyringUpdate, PairingStart, PairingStatus};
use spoko_server::test_util::{TestServer, spawn_test_server};
use tungstenite::Message;
use uuid::Uuid;

fn first_device(srv: &TestServer, account: &str) -> (tempfile::TempDir, Identity) {
    let home = tempfile::tempdir().unwrap();
    let token = srv.bootstrap_token(account);
    let (id, _) = account::login_bootstrap(home.path(), &srv.url, &token, "a", None).unwrap();
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

fn pairing_start(api: &ApiClient) -> spoko_proto::PairingStarted {
    let req = PairingStart {
        device_name: "new".into(),
        device_pubkey: DeviceKeypair::generate().public(),
    };
    api.pairing_start(&req).unwrap()
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
    assert!(is_unauthorized(bad.my_keyring()));
    assert!(is_unauthorized(bad.pairing_by_code("ABCD2345")));
    assert!(is_unauthorized(
        bad.pairing_approve(Uuid::new_v4(), vec![1])
    ));
    assert!(is_unauthorized(bad.create_project(b"m")));
    assert!(is_unauthorized(bad.get_pointer(p, "head")));
    assert!(is_unauthorized(bad.put_pointer(p, "head", None, b"v")));
    assert!(is_unauthorized(bad.post_message(p, b"hello")));
    assert!(is_unauthorized(bad.presence(p)));
    let upd = KeyringUpdate {
        per_device: vec![],
        recovery_wrapped_keyring: None,
    };
    assert!(is_unauthorized(bad.update_keyrings(&upd)));
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
fn spent_bootstrap_token_cannot_log_in_again() {
    let srv = spawn_test_server();
    let token = srv.bootstrap_token("jan");
    let home = tempfile::tempdir().unwrap();
    account::login_bootstrap(home.path(), &srv.url, &token, "a", None).unwrap();
    let again = tempfile::tempdir().unwrap();
    let r = account::login_bootstrap(again.path(), &srv.url, &token, "b", None);
    assert!(is_unauthorized(r));
    assert!(Identity::load(again.path()).is_err(), "no identity written");
}

#[test]
fn unreachable_server_is_a_network_error() {
    let r = account::pair_start("http://127.0.0.1:1", "new");
    assert!(matches!(r, Err(ClientError::Network(_))));
}

#[test]
fn pairing_poll_reports_delivered_and_unknown_pairings() {
    let srv = spawn_test_server();
    let (_h, id) = first_device(&srv, "jan");
    let anon = ApiClient::new(&srv.url, None);
    let started = pairing_start(&anon);
    let poll = || {
        anon.pairing_poll(started.pairing_id, &started.secret)
            .unwrap()
    };
    assert!(matches!(poll(), PollResult::Status(PairingStatus::Pending)));
    id.api()
        .pairing_approve(started.pairing_id, vec![9; 8])
        .unwrap();
    match poll() {
        PollResult::Status(PairingStatus::Approved {
            wrapped_keyring, ..
        }) => {
            assert_eq!(wrapped_keyring, vec![9; 8]);
        }
        _ => panic!("expected approval"),
    }
    assert!(
        matches!(poll(), PollResult::Gone),
        "credentials are handed out once"
    );
    let unknown = anon.pairing_poll(Uuid::new_v4(), "secret");
    assert!(matches!(unknown, Err(ClientError::Other(m)) if m == "pairing not found"));
    let wrong_secret = anon.pairing_poll(started.pairing_id, "nope");
    assert!(matches!(wrong_secret, Err(ClientError::Other(m)) if m == "pairing not found"));
}

#[test]
fn pairing_expires_while_waiting() {
    let srv = spawn_test_server();
    let (_h, id) = first_device(&srv, "jan");
    let pending = account::pair_start(&srv.url, "new").unwrap();
    let code = pending.code.clone();
    srv.state
        .db
        .execute_raw("UPDATE pairings SET expires_at = 0")
        .unwrap();
    let r = account::pair_approve(&id, &code, |_, _| true);
    assert!(matches!(r, Err(ClientError::Other(m)) if m.contains("no pending pairing")));
    let home = tempfile::tempdir().unwrap();
    let err = pending.wait(
        home.path(),
        Duration::from_secs(5),
        Duration::from_millis(10),
    );
    assert!(matches!(err, Err(ClientError::Other(m)) if m == "pairing expired"));
    assert!(Identity::load(home.path()).is_err());
}

#[test]
fn approval_fails_when_pairing_expires_during_confirmation() {
    let srv = spawn_test_server();
    let (_h, id) = first_device(&srv, "jan");
    let pending = account::pair_start(&srv.url, "new").unwrap();
    let r = account::pair_approve(&id, &pending.code, |name, _| {
        assert_eq!(name, "new");
        srv.state
            .db
            .execute_raw("UPDATE pairings SET expires_at = 0")
            .unwrap();
        true
    });
    assert_eq!(server_error(r), "HTTP 404: ");
    assert_eq!(id.api().devices().unwrap().len(), 1, "no device was added");
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
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("not-a-dir");
    std::fs::write(&home, b"x").unwrap();
    let r = account::login_bootstrap(&home, &srv.url, &srv.bootstrap_token("jan"), "a", None);
    assert!(matches!(r, Err(ClientError::Io(_))), "{:?}", r.map(|_| ()));
}

#[test]
fn recovery_login_needs_a_stored_and_valid_keyring() {
    let srv = spawn_test_server();
    let rk = RecoveryKey::generate();
    let home = tempfile::tempdir().unwrap();
    // an account whose first device never stored a recovery blob
    let token = srv.bootstrap_token("empty");
    let r = account::login_bootstrap(home.path(), &srv.url, &token, "b", Some(&rk));
    assert!(matches!(r, Err(ClientError::Other(m)) if m.contains("no recovery key stored")));
    // a blob the key opens, but which holds no keyring
    let acc = srv.state.db.account_by_name("empty").unwrap().unwrap();
    srv.state
        .db
        .set_recovery(acc, &rk.wrap(b"not a keyring"))
        .unwrap();
    let r = account::login_bootstrap(home.path(), &srv.url, &token, "b", Some(&rk));
    assert!(
        matches!(r, Err(ClientError::Crypto(_))),
        "{:?}",
        r.map(|_| ())
    );
    assert!(Identity::load(home.path()).is_err());
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
