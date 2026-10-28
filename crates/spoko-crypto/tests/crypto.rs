use spoko_crypto::*;

#[test]
fn seal_open_roundtrip() {
    let kr = Keyring::generate();
    let sealed = seal(&kr, b"id", b"hello");
    assert_eq!(open(&kr, b"id", &sealed).unwrap(), b"hello");
}

#[test]
fn open_rejects_wrong_aad() {
    let kr = Keyring::generate();
    let sealed = seal(&kr, b"id", b"hello");
    assert!(open(&kr, b"other", &sealed).is_err());
}

#[test]
fn open_rejects_truncated_and_unknown_version() {
    let kr = Keyring::generate();
    assert!(matches!(open(&kr, b"", b"abc"), Err(CryptoError::Malformed)));
    let mut sealed = seal(&kr, b"", b"x");
    sealed[0] = 99;
    assert!(matches!(open(&kr, b"", &sealed), Err(CryptoError::UnknownKeyVersion(99))));
}

#[test]
fn open_after_rotate_reads_old_version() {
    let mut kr = Keyring::generate();
    let old = seal(&kr, b"a", b"old");
    assert_eq!(kr.rotate(), 2);
    assert_eq!(open(&kr, b"a", &old).unwrap(), b"old");
    let new = seal(&kr, b"a", b"new");
    assert_eq!(u32::from_le_bytes(new[0..4].try_into().unwrap()), 2);
    assert_eq!(kr.current().0, 2);
}

#[test]
fn keyring_bytes_roundtrip() {
    let mut kr = Keyring::generate();
    kr.rotate();
    let back = Keyring::from_bytes(&kr.to_bytes()).unwrap();
    assert_eq!(back, kr);
    assert!(Keyring::from_bytes(b"junk").is_err());
}

#[test]
fn ids_are_keyed() {
    let a = Keyring::generate();
    let b = Keyring::generate();
    let oid = [7u8; 20];
    assert_ne!(git_object_id(&a, &oid), git_object_id(&b, &oid));
    assert_eq!(git_object_id(&a, &oid), git_object_id(&a, &oid));
    assert_ne!(chunk_id(&a, b"x"), chunk_id(&b, b"x"));
    assert_ne!(chunk_id(&a, b"x"), git_object_id(&a, b"x"));
    assert_eq!(name_id(&a, "vol", "pg"), name_id(&a, "vol", "pg"));
    assert_ne!(name_id(&a, "vol", "pg"), name_id(&a, "ptr", "pg"));
    assert_eq!(name_id(&a, "vol", "pg").len(), 64);
}

#[test]
fn object_id_hex_roundtrip() {
    let kr = Keyring::generate();
    let id = chunk_id(&kr, b"abc");
    let s = id.to_string();
    assert_eq!(s.len(), 64);
    assert_eq!(s.parse::<ObjectId>().unwrap(), id);
    assert!("zz".parse::<ObjectId>().is_err());
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, format!("\"{s}\""));
    assert_eq!(serde_json::from_str::<ObjectId>(&json).unwrap(), id);
    assert!(serde_json::from_str::<ObjectId>("\"00\"").is_err());
}

#[test]
fn sealed_box_roundtrip() {
    let kp = DeviceKeypair::generate();
    let sealed = seal_to(&kp.public(), b"keyring");
    assert_eq!(kp.unseal(&sealed).unwrap(), b"keyring");
    let restored = DeviceKeypair::from_secret_bytes(kp.secret_bytes());
    assert_eq!(restored.public(), kp.public());
}

#[test]
fn unseal_with_other_key_fails() {
    let kp = DeviceKeypair::generate();
    let other = DeviceKeypair::generate();
    let sealed = seal_to(&kp.public(), b"keyring");
    assert!(other.unseal(&sealed).is_err());
}

#[test]
fn fingerprint_format() {
    let kp = DeviceKeypair::generate();
    let fp = fingerprint(&kp.public());
    assert_eq!(fp.len(), 14);
    let parts: Vec<&str> = fp.split('-').collect();
    assert_eq!(parts.len(), 3);
    assert!(parts.iter().all(|p| p.len() == 4 && p.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())));
}

#[test]
fn recovery_display_parse_roundtrip() {
    let rk = RecoveryKey::generate();
    let shown = rk.to_display();
    assert!(shown.contains('-'));
    assert_eq!(RecoveryKey::parse(&shown).unwrap(), rk);
    assert_eq!(RecoveryKey::parse(&shown.to_lowercase().replace('-', " ")).unwrap(), rk);
    assert!(RecoveryKey::parse("AAAA").is_err());
    assert!(RecoveryKey::parse("!!!!").is_err());
}

#[test]
fn recovery_wrap_unwrap() {
    let rk = RecoveryKey::generate();
    let wrapped = rk.wrap(b"keyring-bytes");
    assert_eq!(rk.unwrap(&wrapped).unwrap(), b"keyring-bytes");
}

#[test]
fn recovery_wrong_key_fails() {
    let wrapped = RecoveryKey::generate().wrap(b"k");
    assert!(RecoveryKey::generate().unwrap(&wrapped).is_err());
    assert!(RecoveryKey::generate().unwrap(b"short").is_err());
}

#[test]
fn private_file_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("sub/key.bin");
    write_private_file(&p, b"secret").unwrap();
    assert_eq!(read_private_file(&p).unwrap(), b"secret");
    write_private_file(&p, b"secret2").unwrap();
    assert_eq!(read_private_file(&p).unwrap(), b"secret2");
}

#[cfg(unix)]
#[test]
fn private_file_mode_0600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("key.bin");
    write_private_file(&p, b"secret").unwrap();
    assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn debug_output_hides_key_material() {
    let kr = Keyring::generate();
    assert_eq!(format!("{kr:?}"), "Keyring(versions: [1])");
    let id = chunk_id(&kr, b"a");
    assert_eq!(format!("{id:?}"), format!("ObjectId({id})"));
}

#[test]
fn empty_keyring_bytes_rejected() {
    let mut kr = Keyring::generate();
    kr.keys.clear();
    let bytes = bincode_bytes(&kr);
    assert!(Keyring::from_bytes(&bytes).is_err());
}

fn bincode_bytes(kr: &Keyring) -> Vec<u8> {
    kr.to_bytes()
}
