#![cfg_attr(windows, allow(unused_imports, dead_code))] // helpers of #[cfg(unix)] tests
#[cfg(unix)]
use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::time::Duration;

use serde_json::json;
use spoko_proto::{DeviceCredentials, ServerEvent};
use spoko_server::blobstore::BlobStore;
use spoko_server::test_util::spawn_test_server;
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};
use uuid::Uuid;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().http_status_as_error(false).build().into()
}

#[test]
fn blob_store_refuses_a_corrupt_index() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.db"), vec![0x5a; 4096]).unwrap();
    let err = BlobStore::open(dir.path()).err().expect("a garbage index.db must not open");
    assert_eq!(err.kind(), std::io::ErrorKind::Other);
    assert!(err.to_string().contains("not a database"), "{err}");
}

type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

fn read_event(ws: &mut Ws) -> ServerEvent {
    loop {
        if let Message::Text(t) = ws.read().unwrap() {
            return serde_json::from_str(&t).unwrap();
        }
    }
}

#[test]
fn lagging_socket_skips_missed_events_and_stays_open() {
    let srv = spawn_test_server();
    let token = srv.bootstrap_token("jan");
    let body = json!({"device_name":"d","device_pubkey":"AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=","wrapped_keyring":"AQ=="});
    let creds: DeviceCredentials = agent()
        .post(format!("{}/v1/devices/bootstrap", srv.url))
        .header("Authorization", format!("Bearer {token}"))
        .send_json(body)
        .unwrap()
        .body_mut()
        .read_json()
        .unwrap();
    let mut req = format!("ws://{}/v1/events", srv.addr).into_client_request().unwrap();
    req.headers_mut().insert("Authorization", format!("Bearer {}", creds.token).parse().unwrap());
    let (mut ws, _) = tungstenite::connect(req).unwrap();
    if let MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    }
    let account = srv.state.db.account_by_name("jan").unwrap().unwrap();
    let project = Uuid::new_v4();
    // the client reads nothing while far more events than the channel holds (1024) are published
    let published = 20_000u32;
    for i in 0..published {
        srv.state.hub.publish(account, None, ServerEvent::Message { project, from_device: Uuid::nil(), payload: [i.to_le_bytes().to_vec(), vec![0u8; 2048]].concat() });
    }
    srv.state.hub.publish(account, None, ServerEvent::Message { project, from_device: Uuid::nil(), payload: b"last".to_vec() });
    let mut received = 0u32;
    loop {
        match read_event(&mut ws) {
            ServerEvent::Message { payload, .. } if payload == b"last" => break,
            ServerEvent::Message { .. } => received += 1,
            _ => {}
        }
    }
    assert!(received < published, "a lagging socket drops what it missed ({received} of {published} delivered)");
}

#[cfg(unix)]
#[test]
fn serve_binary_shuts_down_cleanly_on_ctrl_c() {
    let dir = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_spoko-server"))
        .args(["serve", "--data", dir.path().to_str().unwrap(), "--listen", &format!("127.0.0.1:{port}")])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let first = lines.next().unwrap().unwrap();
    assert!(first.contains(&format!("listening on 127.0.0.1:{port}")), "{first}");
    let url = format!("http://127.0.0.1:{port}/v1/devices");
    assert_eq!(agent().get(&url).call().unwrap().status().as_u16(), 401);
    // the shutdown future is polled once the server is accepting; give it a moment to register
    std::thread::sleep(Duration::from_millis(200));
    let kill = std::process::Command::new("kill").args(["-INT", &child.id().to_string()]).status().unwrap();
    assert!(kill.success());
    let status = child.wait().unwrap();
    assert!(status.success(), "SIGINT is a graceful shutdown, not a crash: {status:?}");
    assert!(dir.path().join("spoko.db").exists());
}

#[test]
fn serve_binary_exits_1_when_the_port_is_taken() {
    let dir = tempfile::tempdir().unwrap();
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = busy.local_addr().unwrap().to_string();
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_spoko-server")).args(["serve", "--data", dir.path().to_str().unwrap(), "--listen", &addr]).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.to_lowercase().contains("address") && err.to_lowercase().contains("in use"), "{err}");
}
