use std::net::TcpStream;
use std::time::Duration;

use serde_json::json;
use spoko_proto::*;
use spoko_server::test_util::{TestServer, spawn_test_server};
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};
use uuid::Uuid;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).build().into()
}

fn device(srv: &TestServer, key: &str) -> spoko_server::db::DeviceLogin {
    srv.login(key, "d")
}

fn create_project(srv: &TestServer, token: &str) -> Uuid {
    let p: ProjectInfo = agent()
        .post(format!("{}/v1/projects", srv.url))
        .header("Authorization", format!("Bearer {token}"))
        .send_json(json!({"encrypted_meta":"AQ=="}))
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    p.id
}

type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

fn connect(srv: &TestServer, token: &str) -> Ws {
    let mut req = format!("ws://{}/v1/events", srv.addr).into_client_request().unwrap();
    req.headers_mut().insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (ws, _) = tungstenite::connect(req).unwrap();
    if let MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    }
    ws
}

fn next_event(ws: &mut Ws) -> ServerEvent {
    loop {
        match ws.read().unwrap() {
            Message::Text(t) => return serde_json::from_str(&t).unwrap(),
            _ => continue,
        }
    }
}

fn next_non_presence(ws: &mut Ws) -> ServerEvent {
    loop {
        let e = next_event(ws);
        if !matches!(e, ServerEvent::PresenceChanged { .. }) {
            return e;
        }
    }
}

fn put_pointer(srv: &TestServer, token: &str, p: Uuid) {
    let status = agent()
        .put(format!("{}/v1/projects/{p}/pointers/head", srv.url))
        .header("Authorization", format!("Bearer {token}"))
        .send_json(json!({"expected_version":null,"value":"AQ=="}))
        .unwrap()
        .status();
    assert_eq!(status, 200);
}

#[test]
fn ws_requires_auth() {
    let srv = spawn_test_server();
    let req = format!("ws://{}/v1/events", srv.addr).into_client_request().unwrap();
    assert!(tungstenite::connect(req).is_err());
}

#[test]
fn pointer_change_notifies_other_device() {
    let srv = spawn_test_server();
    let a = device(&srv, "id_ed25519");
    let b = device(&srv, "id_ed25519");
    let p = create_project(&srv, &a.token);
    let mut wb = connect(&srv, &b.token);
    let _ = next_event(&mut wb); // own presence
    put_pointer(&srv, &a.token, p);
    assert_eq!(next_non_presence(&mut wb), ServerEvent::PointerChanged { project: p, name: "head".into(), version: 1 });
}

#[test]
fn message_not_echoed_to_sender() {
    let srv = spawn_test_server();
    let a = device(&srv, "id_ed25519");
    let b = device(&srv, "id_ed25519");
    let p = create_project(&srv, &a.token);
    let mut wa = connect(&srv, &a.token);
    let _ = next_event(&mut wa);
    let mut wb = connect(&srv, &b.token);
    let _ = next_event(&mut wb);
    let status = agent()
        .post(format!("{}/v1/projects/{p}/messages", srv.url))
        .header("Authorization", format!("Bearer {}", a.token))
        .send_json(json!({"payload":"Bw=="}))
        .unwrap()
        .status();
    assert_eq!(status, 204);
    assert_eq!(next_non_presence(&mut wb), ServerEvent::Message { project: p, from_device: a.device_id, payload: vec![7] });
    // A then changes a pointer; the first non-presence event A sees must not be its own message.
    put_pointer(&srv, &b.token, p);
    assert!(matches!(next_non_presence(&mut wa), ServerEvent::PointerChanged { .. }));
}

#[test]
fn other_account_events_are_isolated() {
    let srv = spawn_test_server();
    let a = device(&srv, "id_ed25519");
    let b = device(&srv, "other_ed25519");
    let b2 = device(&srv, "other_ed25519");
    let pa = create_project(&srv, &a.token);
    let pb = create_project(&srv, &b.token);
    let mut wb = connect(&srv, &b.token);
    let _ = next_event(&mut wb); // first frame = presence ⇒ subscription active
    put_pointer(&srv, &a.token, pa);
    put_pointer(&srv, &b2.token, pb);
    assert_eq!(next_non_presence(&mut wb), ServerEvent::PointerChanged { project: pb, name: "head".into(), version: 1 });
}

#[test]
fn presence_reflects_ws_connections() {
    let srv = spawn_test_server();
    let a = device(&srv, "id_ed25519");
    let b = device(&srv, "id_ed25519");
    let p = create_project(&srv, &a.token);
    let presence = |token: &str| -> Presence {
        agent()
            .get(format!("{}/v1/projects/{p}/presence", srv.url))
            .header("Authorization", format!("Bearer {token}"))
            .call()
            .unwrap()
            .body_mut()
            .read_json()
            .unwrap()
    };
    assert!(presence(&a.token).online_devices.is_empty());
    let mut wa = connect(&srv, &a.token);
    assert_eq!(next_event(&mut wa), ServerEvent::PresenceChanged { project: p, online_devices: vec![a.device_id] });
    let wb = connect(&srv, &b.token);
    let mut expected = vec![a.device_id, b.device_id];
    expected.sort();
    assert_eq!(next_event(&mut wa), ServerEvent::PresenceChanged { project: p, online_devices: expected.clone() });
    assert_eq!(presence(&a.token).online_devices, expected);
    drop(wb);
    assert_eq!(next_event(&mut wa), ServerEvent::PresenceChanged { project: p, online_devices: vec![a.device_id] });
}
