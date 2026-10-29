use std::path::{Path, PathBuf};

use spoko_crypto::{CryptoError, Keyring, SshKey, default_key_path, verify_signature};

fn key(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/keys").join(name)
}

fn load(name: &str) -> SshKey {
    SshKey::load(&key(name), None).unwrap()
}

#[test]
fn same_key_same_master_across_comment_and_reencode() {
    let a = load("id_ed25519");
    let b = load("id_ed25519_recomment");
    assert_eq!(a.master(), b.master(), "the comment is not key material");
    assert_eq!(a.fingerprint(), b.fingerprint());
    assert_eq!(load("id_ed25519").master(), a.master(), "deterministic");
}

#[test]
fn different_keys_different_master() {
    let masters: Vec<_> = ["id_ed25519", "other_ed25519", "id_ecdsa", "id_rsa"].iter().map(|k| load(k).master()).collect();
    for (i, m) in masters.iter().enumerate() {
        assert!(masters.iter().skip(i + 1).all(|o| o != m));
    }
}

#[test]
fn fingerprint_matches_ssh_keygen_format() {
    let k = load("id_ed25519");
    assert!(k.fingerprint().starts_with("SHA256:"), "{}", k.fingerprint());
    let public = std::fs::read_to_string(key("id_ed25519.pub")).unwrap();
    assert!(public.starts_with(&k.public_openssh()), "public part without the comment");
    assert!(k.public_openssh().starts_with("ssh-ed25519 "));
}

#[test]
fn sign_verify_roundtrip_each_type() {
    for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
        let k = load(name);
        let sig = k.sign(b"challenge-123").unwrap_or_else(|e| panic!("{name} sign: {e}"));
        verify_signature(&k.public_openssh(), b"challenge-123", &sig).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(verify_signature(&k.public_openssh(), b"challenge-124", &sig).is_err(), "{name}: other message");
        assert!(verify_signature(&load("other_ed25519").public_openssh(), b"challenge-123", &sig).is_err(), "{name}: other key");
    }
}

#[test]
fn tampered_or_garbage_signature_rejected() {
    let k = load("id_ed25519");
    let sig = k.sign(b"m").unwrap();
    let tampered = sig.replacen('A', "B", 1);
    assert!(verify_signature(&k.public_openssh(), b"m", &tampered).is_err());
    assert!(verify_signature(&k.public_openssh(), b"m", "not a signature").is_err());
    assert!(verify_signature("ssh-ed25519 garbage", b"m", &sig).is_err());
}

#[test]
fn encrypted_key_needs_passphrase() {
    assert!(matches!(SshKey::load(&key("id_ed25519_pass"), None), Err(CryptoError::Encrypted)));
    assert!(matches!(SshKey::load(&key("id_ed25519_pass"), Some("wrong")), Err(CryptoError::Passphrase)));
    let k = SshKey::load(&key("id_ed25519_pass"), Some("sekret")).unwrap();
    assert!(SshKey::is_encrypted_file(&key("id_ed25519_pass")).unwrap());
    assert!(!SshKey::is_encrypted_file(&key("id_ed25519")).unwrap());
    let sig = k.sign(b"x").unwrap();
    verify_signature(&k.public_openssh(), b"x", &sig).unwrap();
    assert!(matches!(SshKey::load(&key("missing"), None), Err(CryptoError::Io(_))));
    assert!(matches!(SshKey::load(&key("README.md"), None), Err(CryptoError::Encoding)));
}

#[test]
fn wrap_unwrap_keyring() {
    let k = load("id_ed25519");
    let kr = Keyring::generate();
    let wrapped = k.wrap_keyring(&kr);
    let back = load("id_ed25519_recomment").unwrap_keyring(&wrapped).unwrap();
    assert_eq!(back.to_bytes(), kr.to_bytes());
    assert!(matches!(load("other_ed25519").unwrap_keyring(&wrapped), Err(CryptoError::Decrypt)));
}

#[test]
fn default_key_order() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(default_key_path(home.path()), None);
    let ssh = home.path().join(".ssh");
    std::fs::create_dir_all(&ssh).unwrap();
    std::fs::write(ssh.join("id_rsa"), "x").unwrap();
    assert_eq!(default_key_path(home.path()), Some(ssh.join("id_rsa")));
    std::fs::write(ssh.join("id_ecdsa"), "x").unwrap();
    assert_eq!(default_key_path(home.path()), Some(ssh.join("id_ecdsa")));
    std::fs::write(ssh.join("id_ed25519"), "x").unwrap();
    assert_eq!(default_key_path(home.path()), Some(ssh.join("id_ed25519")));
}


#[test]
fn signatures_verify_with_openssh() {
    if std::process::Command::new("ssh-keygen").arg("-?").output().is_err() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    for name in ["id_ed25519", "id_ecdsa", "id_rsa"] {
        let sig = dir.path().join(format!("{name}.sig"));
        std::fs::write(&sig, load(name).sign(b"hello spoko").unwrap()).unwrap();
        let mut child = std::process::Command::new("ssh-keygen")
            .args(["-Y", "check-novalidate", "-n", "spoko", "-s"])
            .arg(&sig)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(&mut child.stdin.take().unwrap(), b"hello spoko").unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn public_key_lines_parse_without_comment() {
    let line = std::fs::read_to_string(key("id_ed25519.pub")).unwrap();
    let (public, fp) = spoko_crypto::parse_public_key(&line).unwrap();
    let k = load("id_ed25519");
    assert_eq!(public, k.public_openssh());
    assert_eq!(fp, k.fingerprint());
    assert!(spoko_crypto::parse_public_key("ssh-ed25519 nope").is_err());
}
