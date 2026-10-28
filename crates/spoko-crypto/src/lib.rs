//! Cryptography for spoko: the server only ever sees what these functions produce.

mod aead;
mod ids;
mod keyfile;
mod keyring;
#[cfg(windows)]
mod os_glue;
mod recovery;
mod sealed;

pub use aead::{open, seal};
pub use ids::{ObjectId, chunk_id, git_object_id, name_id};
pub use keyfile::{read_private_file, write_private_file};
pub use keyring::{Key32, Keyring};
pub use recovery::RecoveryKey;
pub use sealed::{DeviceKeypair, fingerprint, seal_to};

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("malformed ciphertext")]
    Malformed,
    #[error("unknown key version {0}")]
    UnknownKeyVersion(u32),
    #[error("decryption failed")]
    Decrypt,
    #[error("invalid encoding")]
    Encoding,
}

pub(crate) fn random_bytes<const N: usize>() -> [u8; N] {
    use rand_core::RngCore;
    let mut out = [0u8; N];
    rand_core::OsRng.fill_bytes(&mut out);
    out
}
