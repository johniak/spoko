//! SSH keys for tests (pre-generated with `ssh-keygen`, see `tests/keys/README.md`).

use std::path::PathBuf;

use crate::SshKey;

/// `id_ed25519`, `other_ed25519`, `id_ecdsa`, `id_rsa`, `id_ed25519_pass` (passphrase `sekret`), …
pub fn key_path(name: &str) -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/keys")).join(name)
}

pub fn key(name: &str) -> SshKey {
    SshKey::load(&key_path(name), None).expect("test key loads")
}
