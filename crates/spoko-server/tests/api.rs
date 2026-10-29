use serde_json::{Value, json};
use spoko_crypto::ObjectId;
use spoko_proto::*;
use spoko_crypto::test_util;
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

/// Logs a device in with a test SSH key (the account is created for the key if needed).
fn login(srv: &TestServer, key: &str, device: &str) -> LoginResponse {
    let k = test_util::key(key);
    srv.create_account(key, &k);
    let anon = Http::new(srv, None);
    let (s, v) = anon.json("POST", "/auth/challenge", json!({"public_key": k.public_openssh()}));
    assert_eq!(s, 200, "{v}");
    let c: Challenge = serde_json::from_value(v).unwrap();
    let req = LoginRequest { challenge_id: c.challenge_id, signature: k.sign(&c.nonce).unwrap(), device_name: device.into(), device_id: None };
    let (s, v) = anon.json("POST", "/auth/login", serde_json::to_value(req).unwrap());
    assert_eq!(s, 200, "{v}");
    serde_json::from_value(v).unwrap()
}

fn challenge(srv: &TestServer, key: &str) -> Challenge {
    let public = test_util::key(key).public_openssh();
    serde_json::from_value(Http::new(srv, None).json("POST", "/auth/challenge", json!({"public_key": public})).1).unwrap()
}

fn try_login(srv: &TestServer, c: &Challenge, signature: String) -> u16 {
    let req = LoginRequest { challenge_id: c.challenge_id, signature, device_name: "x".into(), device_id: None };
    Http::new(srv, None).json("POST", "/auth/login", serde_json::to_value(req).unwrap()).0
}

#[test]
fn login_with_signed_challenge() {
    let srv = spawn_test_server();
    let creds = login(&srv, "id_ed25519", "mac");
    assert_eq!(creds.keyring, None, "a new account has no keyring yet");
    let (s, body) = Http::new(&srv, Some(&creds.token)).get("/devices");
    assert_eq!(s, 200);
    let devices: Vec<DeviceInfo> = serde_json::from_slice(&body).unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].name, "mac");
    assert!(devices[0].last_seen.is_some());
    assert_eq!(Http::new(&srv, None).get("/devices").0, 401);
    assert_eq!(Http::new(&srv, Some("bogus")).get("/devices").0, 401);
    // every key type works
    for key in ["id_ecdsa", "id_rsa"] {
        assert!(!login(&srv, key, "linux").token.is_empty());
    }
}

#[test]
fn second_login_same_key_same_account_new_device() {
    let srv = spawn_test_server();
    let a = login(&srv, "id_ed25519", "mac");
    let b = login(&srv, "id_ed25519", "linux");
    assert_eq!(a.account_id, b.account_id);
    assert_ne!(a.device_id, b.device_id);
    // logging in again as a known device keeps its id and replaces its token
    let k = test_util::key("id_ed25519");
    let c = challenge(&srv, "id_ed25519");
    let req = LoginRequest { challenge_id: c.challenge_id, signature: k.sign(&c.nonce).unwrap(), device_name: "mac2".into(), device_id: Some(a.device_id) };
    let again: LoginResponse = serde_json::from_value(Http::new(&srv, None).json("POST", "/auth/login", serde_json::to_value(req).unwrap()).1).unwrap();
    assert_eq!(again.device_id, a.device_id);
    assert_eq!(Http::new(&srv, Some(&a.token)).get("/devices").0, 401, "old token replaced");
    assert_eq!(Http::new(&srv, Some(&again.token)).get("/devices").0, 200);
}

#[test]
fn challenge_single_use_and_bound_to_key() {
    let srv = spawn_test_server();
    login(&srv, "id_ed25519", "mac");
    let k = test_util::key("id_ed25519");
    let c = challenge(&srv, "id_ed25519");
    let sig = k.sign(&c.nonce).unwrap();
    assert_eq!(try_login(&srv, &c, sig.clone()), 200);
    assert_eq!(try_login(&srv, &c, sig), 401, "used up");
    // a challenge issued for another key cannot be answered with this one
    let other = challenge(&srv, "other_ed25519");
    assert_eq!(try_login(&srv, &other, k.sign(&other.nonce).unwrap()), 401);
    // nor with a signature of something else
    let c = challenge(&srv, "id_ed25519");
    assert_eq!(try_login(&srv, &c, k.sign(b"other").unwrap()), 401);
    let bad = Challenge { challenge_id: Uuid::new_v4(), nonce: vec![] };
    assert_eq!(try_login(&srv, &bad, "x".into()), 401);
}

#[test]
fn unknown_key_gets_a_challenge_but_no_login() {
    let srv = spawn_test_server();
    let k = test_util::key("other_ed25519");
    let c = challenge(&srv, "other_ed25519");
    assert_eq!(try_login(&srv, &c, k.sign(&c.nonce).unwrap()), 401);
    assert_eq!(Http::new(&srv, None).json("POST", "/auth/challenge", json!({"public_key": "ssh-ed25519 nope"})).0, 400);
}

#[test]
fn initial_keyring_only_once() {
    let srv = spawn_test_server();
    let a = login(&srv, "id_ed25519", "mac");
    let h = Http::new(&srv, Some(&a.token));
    assert_eq!(h.get("/account/keyring").0, 404);
    let kr = json!({"ssh_wrapped": "AQI=", "recovery_wrapped": "CQ=="});
    assert_eq!(h.json("PUT", "/account/keyring", kr.clone()).0, 204);
    assert_eq!(h.json("PUT", "/account/keyring", kr).0, 409, "already wrapped for this key");
    let got: AccountKeyring = serde_json::from_slice(&h.get("/account/keyring").1).unwrap();
    assert_eq!(got.ssh_wrapped, vec![1, 2]);
    assert_eq!(got.recovery_wrapped, Some(vec![9]));
    assert_eq!(got.wrapped_for, test_util::key("id_ed25519").fingerprint());
    // the next machine gets it at login
    assert_eq!(login(&srv, "id_ed25519", "linux").keyring, Some(got));
}

#[test]
fn replaced_key_may_store_a_new_keyring_keeping_recovery() {
    let srv = spawn_test_server();
    let a = login(&srv, "id_ed25519", "mac");
    let h = Http::new(&srv, Some(&a.token));
    assert_eq!(h.json("PUT", "/account/keyring", json!({"ssh_wrapped": "AQI=", "recovery_wrapped": "CQ=="})).0, 204);
    let other = test_util::key("other_ed25519");
    srv.state.db.set_account_key(a.account_id, &other.public_openssh(), &other.fingerprint()).unwrap();
    assert_eq!(h.get("/account/keyring").0, 401, "devices are logged out");
    let b = login(&srv, "other_ed25519", "mac");
    assert_eq!(b.account_id, a.account_id);
    let stale = b.keyring.unwrap();
    assert_ne!(stale.wrapped_for, other.fingerprint(), "still wrapped for the old key");
    let hb = Http::new(&srv, Some(&b.token));
    assert_eq!(hb.json("PUT", "/account/keyring", json!({"ssh_wrapped": "Bw=="})).0, 204);
    let got: AccountKeyring = serde_json::from_slice(&hb.get("/account/keyring").1).unwrap();
    assert_eq!((got.ssh_wrapped, got.recovery_wrapped, got.wrapped_for), (vec![7], Some(vec![9]), other.fingerprint()));
    assert_eq!(try_login(&srv, &challenge(&srv, "id_ed25519"), test_util::key("id_ed25519").sign(b"x").unwrap()), 401);
}

#[test]
fn pairing_and_bootstrap_routes_gone() {
    let srv = spawn_test_server();
    let a = login(&srv, "id_ed25519", "mac");
    let h = Http::new(&srv, Some(&a.token));
    for path in ["/pairing/start", "/devices/bootstrap"] {
        // `/devices/{id}` only takes DELETE
        assert!(matches!(h.json("POST", path, json!({})).0, 404 | 405), "{path}");
    }
    assert_eq!(h.get("/account/recovery").0, 404);
}

#[test]
fn revoked_device_gets_401() {
    let srv = spawn_test_server();
    let a = login(&srv, "id_ed25519", "a");
    let b = login(&srv, "id_ed25519", "b");
    let ha = Http::new(&srv, Some(&a.token));
    assert_eq!(ha.delete(&format!("/devices/{}", b.device_id)), 204);
    assert_eq!(ha.delete(&format!("/devices/{}", Uuid::new_v4())), 404);
    assert_eq!(Http::new(&srv, Some(&b.token)).get("/devices").0, 401);
}

#[test]
fn other_account_project_is_404() {
    let srv = spawn_test_server();
    let a = login(&srv, "id_ed25519", "a");
    let b = login(&srv, "other_ed25519", "b");
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
    let a = login(srv, "id_ed25519", "a");
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
