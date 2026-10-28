use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

use crate::{CryptoError, Keyring, random_bytes};

const HEADER: usize = 4 + 24;

/// Encrypts with the current key. Layout: `[u32 LE key version][24 B nonce][ciphertext+tag]`.
pub fn seal(kr: &Keyring, aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let (version, key) = kr.current();
    let nonce: [u8; 24] = random_bytes();
    let ct = XChaCha20Poly1305::new(key.into())
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad })
        .expect("encryption cannot fail for in-memory buffers");
    let mut out = Vec::with_capacity(HEADER + ct.len());
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

pub fn open(kr: &Keyring, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if sealed.len() < HEADER {
        return Err(CryptoError::Malformed);
    }
    let version = u32::from_le_bytes(sealed[0..4].try_into().expect("4 bytes"));
    let key = kr.keys.get(&version).ok_or(CryptoError::UnknownKeyVersion(version))?;
    XChaCha20Poly1305::new(key.into())
        .decrypt(XNonce::from_slice(&sealed[4..HEADER]), Payload { msg: &sealed[HEADER..], aad })
        .map_err(|_| CryptoError::Decrypt)
}

/// Symmetric seal with a raw key (recovery key wrapping).
pub(crate) fn seal_raw(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let nonce: [u8; 24] = random_bytes();
    let ct = XChaCha20Poly1305::new(key.into())
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad })
        .expect("encryption cannot fail for in-memory buffers");
    [nonce.as_slice(), &ct].concat()
}

pub(crate) fn open_raw(key: &[u8; 32], aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
    if sealed.len() < 24 {
        return Err(CryptoError::Malformed);
    }
    XChaCha20Poly1305::new(key.into())
        .decrypt(XNonce::from_slice(&sealed[..24]), Payload { msg: &sealed[24..], aad })
        .map_err(|_| CryptoError::Decrypt)
}
