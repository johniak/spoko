use spoko_crypto::ObjectId;
use spoko_proto::*;
use uuid::Uuid;

fn id(b: u8) -> ObjectId {
    ObjectId([b; 32])
}

#[test]
fn batch_roundtrip() {
    let items: Vec<(ObjectId, &[u8])> = vec![(id(1), b"one"), (id(2), b""), (id(3), &[0u8; 300])];
    let bytes = encode_batch(&items);
    let back = decode_batch(&bytes).unwrap();
    assert_eq!(back.len(), 3);
    assert_eq!(back[0], (id(1), b"one".to_vec()));
    assert_eq!(back[1], (id(2), vec![]));
    assert_eq!(back[2].1.len(), 300);
    assert!(decode_batch(&[]).unwrap().is_empty());
}

#[test]
fn batch_truncated_errors() {
    let bytes = encode_batch(&[(id(1), b"hello".as_slice())]);
    assert!(decode_batch(&bytes[..bytes.len() - 1]).is_err());
    assert!(decode_batch(&bytes[..10]).is_err());
    assert!(decode_batch(&bytes[..34]).is_err());
}

#[test]
fn pointer_put_serde_roundtrip() {
    let p = PointerPut { expected_version: Some(3), value: vec![1, 2, 3] };
    let json = serde_json::to_string(&p).unwrap();
    assert_eq!(json, r#"{"expected_version":3,"value":"AQID"}"#);
    assert_eq!(serde_json::from_str::<PointerPut>(&json).unwrap(), p);
    let unconditional: PointerPut = serde_json::from_str(r#"{"expected_version":null,"value":""}"#).unwrap();
    assert_eq!(unconditional.expected_version, None);
    assert!(serde_json::from_str::<PointerPut>(r#"{"expected_version":1,"value":"!!"}"#).is_err());
}

#[test]
fn server_event_tagged_json() {
    let project = Uuid::nil();
    let e = ServerEvent::PointerChanged { project, name: "head".into(), version: 4 };
    let json = serde_json::to_string(&e).unwrap();
    assert!(json.starts_with(r#"{"type":"pointer_changed""#), "{json}");
    assert_eq!(serde_json::from_str::<ServerEvent>(&json).unwrap(), e);
    let m = ServerEvent::Message { project, from_device: Uuid::nil(), payload: vec![7] };
    assert_eq!(serde_json::from_str::<ServerEvent>(&serde_json::to_string(&m).unwrap()).unwrap(), m);
    let p = ServerEvent::PresenceChanged { project, online_devices: vec![Uuid::nil()] };
    assert_eq!(serde_json::from_str::<ServerEvent>(&serde_json::to_string(&p).unwrap()).unwrap(), p);
}

#[test]
fn login_types_roundtrip() {
    let r = LoginResponse {
        account_id: Uuid::nil(),
        device_id: Uuid::nil(),
        token: "t".into(),
        keyring: Some(AccountKeyring { ssh_wrapped: vec![1], wrapped_for: "SHA256:x".into(), recovery_wrapped: None }),
    };
    assert_eq!(serde_json::from_str::<LoginResponse>(&serde_json::to_string(&r).unwrap()).unwrap(), r);
    let c = Challenge { challenge_id: Uuid::nil(), nonce: vec![9; 32] };
    assert_eq!(serde_json::from_str::<Challenge>(&serde_json::to_string(&c).unwrap()).unwrap(), c);
    let l: LoginRequest = serde_json::from_str(r#"{"challenge_id":"00000000-0000-0000-0000-000000000000","signature":"s","device_name":"mac"}"#).unwrap();
    assert_eq!(l.device_id, None);
}

#[test]
fn constants() {
    assert_eq!(MAX_OBJECT_SIZE, 8 * 1024 * 1024);
    assert_eq!(MAX_BATCH_BYTES, 32 * 1024 * 1024);
    assert_eq!(API_PREFIX, "/v1");
}

#[test]
fn optional_bytes_roundtrip() {
    for rec in [None, Some(vec![1u8, 2])] {
        let k = AccountKeyring { ssh_wrapped: vec![3], wrapped_for: String::new(), recovery_wrapped: rec };
        let json = serde_json::to_string(&k).unwrap();
        assert_eq!(serde_json::from_str::<AccountKeyring>(&json).unwrap(), k);
    }
    let missing: AccountKeyring = serde_json::from_str(r#"{"ssh_wrapped":"Aw=="}"#).unwrap();
    assert_eq!((missing.recovery_wrapped, missing.wrapped_for), (None, String::new()));
}
