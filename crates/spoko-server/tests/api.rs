use serde_json::{Value, json};
use spoko_crypto::ObjectId;
use spoko_proto::*;
use spoko_server::test_util::{TestServer, spawn_test_server};
use ureq::Agent;
use uuid::Uuid;

fn agent() -> Agent {
    Agent::config_builder().http_status_as_error(false).build().into()
}

struct Http {
    base: String,
    token: Option<String>,
}

impl Http {
    fn new(srv: &TestServer, token: Option<&str>) -> Http {
        Http { base: format!("{}/v1", srv.url), token: token.map(str::to_owned) }
    }
    fn get(&self, path: &str) -> (u16, Vec<u8>) {
        let mut req = agent().get(format!("{}{path}", self.base));
        if let Some(t) = &self.token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        let mut r = req.call().unwrap();
        (r.status().as_u16(), r.body_mut().with_config().limit(64 << 20).read_to_vec().unwrap())
    }
    fn send(&self, method: &str, path: &str, body: Vec<u8>, ct: &str) -> (u16, Vec<u8>) {
        let url = format!("{}{path}", self.base);
        let a = agent();
        let mut req = match method {
            "POST" => a.post(url),
            "PUT" => a.put(url),
            _ => unreachable!(),
        }
        .header("Content-Type", ct);
        if let Some(t) = &self.token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        let mut r = req.send(&body[..]).unwrap();
        (r.status().as_u16(), r.body_mut().with_config().limit(64 << 20).read_to_vec().unwrap())
    }
    fn json(&self, method: &str, path: &str, v: Value) -> (u16, Value) {
        let (s, b) = self.send(method, path, serde_json::to_vec(&v).unwrap(), "application/json");
        (s, serde_json::from_slice(&b).unwrap_or(Value::Null))
    }
    fn delete(&self, path: &str) -> u16 {
        let mut req = agent().delete(format!("{}{path}", self.base));
        if let Some(t) = &self.token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        req.call().unwrap().status().as_u16()
    }
}

fn bootstrap(srv: &TestServer, account: &str, name: &str) -> DeviceCredentials {
    let token = srv.bootstrap_token(account);
    let req = BootstrapRequest {
        device_name: name.into(),
        device_pubkey: [1; 32],
        wrapped_keyring: vec![1, 2],
        recovery_wrapped_keyring: Some(vec![9]),
    };
    let (s, v) = Http::new(srv, Some(&token)).json("POST", "/devices/bootstrap", serde_json::to_value(req).unwrap());
    assert_eq!(s, 200, "{v}");
    serde_json::from_value(v).unwrap()
}

#[test]
fn bootstrap_then_authenticated_request() {
    let srv = spawn_test_server();
    let creds = bootstrap(&srv, "jan", "mac");
    let (s, body) = Http::new(&srv, Some(&creds.token)).get("/devices");
    assert_eq!(s, 200);
    let devices: Vec<DeviceInfo> = serde_json::from_slice(&body).unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "mac");
    assert!(devices[0].last_seen.is_some());
    let (s, body) = Http::new(&srv, Some(&creds.token)).get("/devices/me/keyring");
    assert_eq!(s, 200);
    let wk: WrappedKeyring = serde_json::from_slice(&body).unwrap();
    assert_eq!(wk.wrapped_keyring, vec![1, 2]);
    assert_eq!(Http::new(&srv, None).get("/devices").0, 401);
    assert_eq!(Http::new(&srv, Some("bogus")).get("/devices").0, 401);
}

#[test]
fn bootstrap_token_is_single_use_and_required() {
    let srv = spawn_test_server();
    let token = srv.bootstrap_token("jan");
    let req = json!({"device_name":"a","device_pubkey":base64_key(),"wrapped_keyring":"AQ=="});
    assert_eq!(Http::new(&srv, Some(&token)).json("POST", "/devices/bootstrap", req.clone()).0, 200);
    assert_eq!(Http::new(&srv, Some(&token)).json("POST", "/devices/bootstrap", req.clone()).0, 401);
    assert_eq!(Http::new(&srv, None).json("POST", "/devices/bootstrap", req).0, 401);
}

fn base64_key() -> String {
    "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=".into()
}

#[test]
fn recovery_requires_bootstrap_token() {
    let srv = spawn_test_server();
    let creds = bootstrap(&srv, "jan", "mac");
    assert_eq!(Http::new(&srv, Some(&creds.token)).get("/account/recovery").0, 401);
    let fresh = srv.bootstrap_token("jan");
    let (s, body) = Http::new(&srv, Some(&fresh)).get("/account/recovery");
    assert_eq!(s, 200);
    let blob: RecoveryBlob = serde_json::from_slice(&body).unwrap();
    assert_eq!(blob.recovery_wrapped_keyring, vec![9]);
    let other = srv.bootstrap_token("nobody-has-recovery");
    assert_eq!(Http::new(&srv, Some(&other)).get("/account/recovery").0, 404);
}

#[test]
fn revoked_device_gets_401() {
    let srv = spawn_test_server();
    let a = bootstrap(&srv, "jan", "a");
    let b = bootstrap(&srv, "jan", "b");
    let ha = Http::new(&srv, Some(&a.token));
    assert_eq!(ha.delete(&format!("/devices/{}", b.device_id)), 204);
    assert_eq!(ha.delete(&format!("/devices/{}", Uuid::new_v4())), 404);
    assert_eq!(Http::new(&srv, Some(&b.token)).get("/devices").0, 401);
}

#[test]
fn keyring_update_rewraps_devices() {
    let srv = spawn_test_server();
    let a = bootstrap(&srv, "jan", "a");
    let h = Http::new(&srv, Some(&a.token));
    let upd = KeyringUpdate {
        per_device: vec![DeviceKeyring { device_id: a.device_id, wrapped_keyring: vec![7] }],
        recovery_wrapped_keyring: Some(vec![8]),
    };
    assert_eq!(h.json("PUT", "/account/keyrings", serde_json::to_value(upd).unwrap()).0, 204);
    let wk: WrappedKeyring = serde_json::from_slice(&h.get("/devices/me/keyring").1).unwrap();
    assert_eq!(wk.wrapped_keyring, vec![7]);
}

#[test]
fn pairing_full_flow() {
    let srv = spawn_test_server();
    let a = bootstrap(&srv, "jan", "a");
    let anon = Http::new(&srv, None);
    let (s, v) = anon.json("POST", "/pairing/start", json!({"device_name":"linux","device_pubkey":base64_key()}));
    assert_eq!(s, 200);
    let started: PairingStarted = serde_json::from_value(v).unwrap();
    let poll = format!("/pairing/{}?secret={}", started.pairing_id, started.secret);
    let (s, body) = anon.get(&poll);
    assert_eq!(s, 200);
    assert_eq!(serde_json::from_slice::<PairingStatus>(&body).unwrap(), PairingStatus::Pending);
    assert_eq!(anon.get(&format!("/pairing/{}?secret=nope", started.pairing_id)).0, 404);

    let ha = Http::new(&srv, Some(&a.token));
    let (s, body) = ha.get(&format!("/pairing/by-code/{}", started.code));
    assert_eq!(s, 200);
    let info: PairingInfo = serde_json::from_slice(&body).unwrap();
    assert_eq!(info.device_name, "linux");
    assert_eq!(ha.get("/pairing/by-code/ZZZZZZZZ").0, 404);
    assert_eq!(anon.get(&format!("/pairing/by-code/{}", started.code)).0, 401);
    let approve = format!("/pairing/{}/approve", started.pairing_id);
    assert_eq!(ha.json("POST", &approve, json!({"wrapped_keyring":"Bw=="})).0, 204);
    assert_eq!(ha.json("POST", &approve, json!({"wrapped_keyring":"Bw=="})).0, 404);

    let (s, body) = anon.get(&poll);
    assert_eq!(s, 200);
    let PairingStatus::Approved { credentials, wrapped_keyring } = serde_json::from_slice(&body).unwrap() else { panic!() };
    assert_eq!(wrapped_keyring, vec![7]);
    assert_eq!(anon.get(&poll).0, 410);
    assert_eq!(Http::new(&srv, Some(&credentials.token)).get("/devices").0, 200);
}

#[test]
fn other_account_project_is_404() {
    let srv = spawn_test_server();
    let a = bootstrap(&srv, "a", "a");
    let b = bootstrap(&srv, "b", "b");
    let ha = Http::new(&srv, Some(&a.token));
    let (s, v) = ha.json("POST", "/projects", json!({"encrypted_meta":"AQ=="}));
    assert_eq!(s, 200);
    let p: ProjectInfo = serde_json::from_value(v).unwrap();
    let list: Vec<ProjectInfo> = serde_json::from_slice(&ha.get("/projects").1).unwrap();
    assert_eq!(list, vec![p.clone()]);
    let hb = Http::new(&srv, Some(&b.token));
    assert_eq!(hb.get(&format!("/projects/{}/pointers/head", p.id)).0, 404);
    assert_eq!(hb.get(&format!("/projects/{}/pointers/head", Uuid::new_v4())).0, 404);
    assert!(serde_json::from_slice::<Vec<ProjectInfo>>(&hb.get("/projects").1).unwrap().is_empty());
}

fn project(srv: &TestServer) -> (Http, Uuid) {
    let a = bootstrap(srv, "jan", "a");
    let h = Http::new(srv, Some(&a.token));
    let p: ProjectInfo = serde_json::from_value(h.json("POST", "/projects", json!({"encrypted_meta":"AQ=="})).1).unwrap();
    (h, p.id)
}

#[test]
fn objects_missing_upload_get() {
    let srv = spawn_test_server();
    let (h, p) = project(&srv);
    let (a, b) = (ObjectId([1; 32]), ObjectId([2; 32]));
    let (s, v) = h.json("POST", &format!("/projects/{p}/objects/missing"), json!({"ids":[a, b]}));
    assert_eq!(s, 200);
    assert_eq!(serde_json::from_value::<MissingResponse>(v).unwrap().ids, vec![a, b]);
    let batch = encode_batch(&[(a, b"aaa".as_slice())]);
    assert_eq!(h.send("POST", &format!("/projects/{p}/objects"), batch, "application/octet-stream").0, 204);
    let (_, v) = h.json("POST", &format!("/projects/{p}/objects/missing"), json!({"ids":[a, b]}));
    assert_eq!(serde_json::from_value::<MissingResponse>(v).unwrap().ids, vec![b]);
    let (s, body) = h.send("POST", &format!("/projects/{p}/objects/get"), serde_json::to_vec(&json!({"ids":[a, b]})).unwrap(), "application/json");
    assert_eq!(s, 200);
    assert_eq!(decode_batch(&body).unwrap(), vec![(a, b"aaa".to_vec())]);
    assert_eq!(h.send("POST", &format!("/projects/{p}/objects"), vec![1, 2, 3], "application/octet-stream").0, 400);
}

#[test]
fn object_too_large_413() {
    let srv = spawn_test_server();
    let (h, p) = project(&srv);
    let big = vec![0u8; MAX_OBJECT_SIZE + 1];
    let batch = encode_batch(&[(ObjectId([3; 32]), big.as_slice())]);
    assert_eq!(h.send("POST", &format!("/projects/{p}/objects"), batch, "application/octet-stream").0, 413);
}

#[test]
fn pointer_cas_409_returns_current() {
    let srv = spawn_test_server();
    let (h, p) = project(&srv);
    let path = format!("/projects/{p}/pointers/head");
    assert_eq!(h.get(&path).0, 404);
    let (s, v) = h.json("PUT", &path, json!({"expected_version":0,"value":"AQ=="}));
    assert_eq!(s, 200);
    assert_eq!(serde_json::from_value::<Pointer>(v).unwrap().version, 1);
    let (s, v) = h.json("PUT", &path, json!({"expected_version":0,"value":"Ag=="}));
    assert_eq!(s, 409);
    assert_eq!(serde_json::from_value::<Pointer>(v).unwrap(), Pointer { version: 1, value: vec![1] });
    let (s, v) = h.json("PUT", &path, json!({"expected_version":null,"value":"Aw=="}));
    assert_eq!((s, serde_json::from_value::<Pointer>(v).unwrap().version), (200, 2));
    let got: Pointer = serde_json::from_slice(&h.get(&path).1).unwrap();
    assert_eq!(got.value, vec![3]);
    assert_eq!(h.json("PUT", &path, json!({"bad":1})).0, 422);
}
