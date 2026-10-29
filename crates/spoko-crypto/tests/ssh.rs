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
    let masters: Vec<_> = ["id_ed25519", "other_ed25519", "id_ecdsa", "id_ecdsa384", "id_ecdsa521", "id_rsa"].iter().map(|k| load(k).master()).collect();
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
    for name in ["id_ed25519", "id_ecdsa", "id_ecdsa384", "id_ecdsa521", "id_rsa"] {
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
    for name in ["id_ed25519", "id_ecdsa", "id_ecdsa384", "id_ecdsa521", "id_rsa"] {
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

#[test]
fn hardware_keys_are_refused() {
    use ssh_key::private::{KeypairData, SkEd25519};
    use ssh_key::public::{Ed25519PublicKey, SkEd25519 as SkPublic};
    let public = SkPublic::new(Ed25519PublicKey([7; 32]), "ssh:");
    let data = KeypairData::SkEd25519(SkEd25519::new(public, 0, vec![1, 2, 3]).unwrap());
    let pem = ssh_key::PrivateKey::new(data, "fido").unwrap().to_openssh(ssh_key::LineEnding::LF).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("id_ed25519_sk");
    std::fs::write(&path, pem.as_bytes()).unwrap();
    assert!(matches!(SshKey::load(&path, None), Err(CryptoError::UnsupportedKey)));
}

#[test]
fn ecdsa_keys_with_a_short_scalar_load() {
    // OpenSSH stores this P-521 scalar in 65 bytes (it starts with a zero byte)
    let plain = load("id_ecdsa521");
    let k = SshKey::load(&key("id_ecdsa521_pass"), Some("sekret")).unwrap();
    assert_eq!((k.master(), k.fingerprint()), (plain.master(), plain.fingerprint()), "the passphrase is not key material");
    verify_signature(&plain.public_openssh(), b"m", &k.sign(b"m").unwrap()).unwrap();
    assert!(matches!(SshKey::load(&key("id_ecdsa521_pass"), Some("wrong")), Err(CryptoError::Passphrase)));
    assert!(matches!(SshKey::load(&key("id_ecdsa521_pass"), None), Err(CryptoError::Encrypted)));
}

#[test]
fn pem_and_pkcs8_keys_ask_for_a_conversion() {
    // `ssh-keygen -m PEM` (the old default for RSA) and `-m PKCS8`
    for name in ["id_rsa_pem", "id_ecdsa_pkcs8"] {
        let path = key(name);
        let e = SshKey::load(&path, None).err().unwrap();
        let p = path.display();
        assert_eq!(e.to_string(), format!("{p} is a PEM/PKCS#8 key; convert it to the OpenSSH format with: ssh-keygen -p -f {p}"), "{name}");
        assert!(matches!(SshKey::is_encrypted_file(&path), Err(CryptoError::NotOpenSsh(_))), "{name}");
    }
}

#[test]
fn public_keys_spoko_cannot_log_in_with_are_refused() {
    for line in [
        "ssh-dss AAAAB3NzaC1kc3MAAAABBQAAAAEFAAAAAQUAAAABBQ== old",
        "sk-ssh-ed25519@openssh.com AAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2guY29tAAAAIAcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHAAAABHNzaDo= yubikey",
    ] {
        assert!(matches!(spoko_crypto::parse_public_key(line), Err(CryptoError::UnsupportedKey)), "{line}");
    }
    for name in ["id_ed25519", "id_ecdsa", "id_ecdsa384", "id_ecdsa521", "id_rsa"] {
        let line = std::fs::read_to_string(key(&format!("{name}.pub"))).unwrap();
        assert_eq!(spoko_crypto::parse_public_key(&line).unwrap().0, load(name).public_openssh());
    }
}
