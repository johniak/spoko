//! The user's SSH key pair: it logs the device in (a signed challenge) and its secret part
//! wraps the account keyring. One key, shared by all of the user's machines.

use std::path::{Path, PathBuf};

use rsa::sha2::Sha512;
use rsa::signature::{SignatureEncoding, Signer};
use ssh_key::private::{KeypairData, RsaKeypair};
use ssh_key::public::KeyData;
use ssh_key::{Algorithm, HashAlg, LineEnding, PrivateKey, PublicKey, Signature, SshSig};
use zeroize::Zeroizing;

use crate::aead::{open_raw, seal_raw};
use crate::{CryptoError, Key32, Keyring};

/// sshsig namespace: a spoko signature is never valid for git, ssh or anything else.
const NAMESPACE: &str = "spoko";
const KDF_CONTEXT: &str = "spoko ssh master v1";
const WRAP_AAD: &[u8] = b"keyring";

/// The default keys, in the order OpenSSH tries them.
const DEFAULT_KEYS: [&str; 3] = ["id_ed25519", "id_ecdsa", "id_rsa"];

/// A decrypted SSH private key and the master key derived from it.
pub struct SshKey {
    key: PrivateKey,
    master: Zeroizing<Key32>,
}

impl SshKey {
    /// Reads an OpenSSH private key file. A key with a passphrase needs `passphrase`
    /// ([`CryptoError::Encrypted`] without one, [`CryptoError::Passphrase`] when wrong).
    pub fn load(path: &Path, passphrase: Option<&str>) -> Result<SshKey, CryptoError> {
        let mut key = read(path)?;
        if key.is_encrypted() {
            let pass = passphrase.ok_or(CryptoError::Encrypted)?;
            key = key.decrypt(pass).map_err(|_| CryptoError::Passphrase)?;
        }
        let master = Zeroizing::new(derive_master(key.key_data())?);
        Ok(SshKey { key, master })
    }

    /// Whether the key file needs a passphrase.
    pub fn is_encrypted_file(path: &Path) -> Result<bool, CryptoError> {
        Ok(read(path)?.is_encrypted())
    }

    /// The public key in `authorized_keys` form, without the comment.
    pub fn public_openssh(&self) -> String {
        public_line(self.key.public_key())
    }

    /// `SHA256:…`, the same as `ssh-keygen -lf`.
    pub fn fingerprint(&self) -> String {
        self.key.fingerprint(HashAlg::Sha256).to_string()
    }

    /// The key that wraps the keyring: derived from the secret part only, so the comment
    /// or the file's encoding do not change it.
    pub fn master(&self) -> Key32 {
        *self.master
    }

    /// An armored sshsig signature of `msg`.
    pub fn sign(&self, msg: &[u8]) -> Result<String, CryptoError> {
        let sig = match self.key.key_data() {
            // ssh-key 0.6 rebuilds RSA keys from (p, p) instead of (p, q): sign through `rsa`
            KeypairData::Rsa(k) => SshSig::sign(&RsaSigner::new(k)?, NAMESPACE, HashAlg::Sha512, msg),
            _ => self.key.sign(NAMESPACE, HashAlg::Sha512, msg),
        }
        .map_err(|_| CryptoError::Signature)?;
        sig.to_pem(LineEnding::LF).map_err(|_| CryptoError::Encoding)
    }

    pub fn wrap_keyring(&self, kr: &Keyring) -> Vec<u8> {
        seal_raw(&self.master, WRAP_AAD, &kr.to_bytes())
    }

    pub fn unwrap_keyring(&self, wrapped: &[u8]) -> Result<Keyring, CryptoError> {
        Keyring::from_bytes(&open_raw(&self.master, WRAP_AAD, wrapped)?)
    }
}

struct RsaSigner {
    key: rsa::pkcs1v15::SigningKey<Sha512>,
    public: KeyData,
}

impl RsaSigner {
    fn new(k: &RsaKeypair) -> Result<RsaSigner, CryptoError> {
        let int = |m: &ssh_key::Mpint| rsa::BigUint::from_bytes_be(m.as_positive_bytes().unwrap_or_default());
        let key = rsa::RsaPrivateKey::from_components(int(&k.public.n), int(&k.public.e), int(&k.private.d), vec![int(&k.private.p), int(&k.private.q)])
            .map_err(|_| CryptoError::Encoding)?;
        Ok(RsaSigner { key: rsa::pkcs1v15::SigningKey::new(key), public: KeyData::Rsa(k.public.clone()) })
    }
}

impl Signer<Signature> for RsaSigner {
    fn try_sign(&self, msg: &[u8]) -> rsa::signature::Result<Signature> {
        let sig = self.key.try_sign(msg)?;
        Signature::new(Algorithm::Rsa { hash: Some(HashAlg::Sha512) }, sig.to_vec()).map_err(|_| rsa::signature::Error::new())
    }
}

impl From<&RsaSigner> for KeyData {
    fn from(s: &RsaSigner) -> KeyData {
        s.public.clone()
    }
}

fn read(path: &Path) -> Result<PrivateKey, CryptoError> {
    let text = std::fs::read(path).map_err(CryptoError::Io)?;
    PrivateKey::from_openssh(text).map_err(|_| CryptoError::Encoding)
}

fn derive_master(data: &KeypairData) -> Result<Key32, CryptoError> {
    let secret: Zeroizing<Vec<u8>> = Zeroizing::new(match data {
        KeypairData::Ed25519(k) => k.private.to_bytes().to_vec(),
        KeypairData::Ecdsa(k) => k.private_key_bytes().to_vec(),
        KeypairData::Rsa(k) => k.private.d.as_bytes().to_vec(),
        // DSA (deprecated) and hardware keys, whose secret never leaves the token
        _ => return Err(CryptoError::UnsupportedKey),
    });
    Ok(blake3::derive_key(KDF_CONTEXT, &secret))
}

fn public_line(key: &PublicKey) -> String {
    PublicKey::new(key.key_data().clone(), "").to_openssh().expect("a parsed key encodes")
}

/// Parses a public key line (`ssh-ed25519 AAAA… comment`) into its comment-free form and
/// fingerprint.
pub fn parse_public_key(line: &str) -> Result<(String, String), CryptoError> {
    let key = PublicKey::from_openssh(line.trim()).map_err(|_| CryptoError::Encoding)?;
    Ok((public_line(&key), key.fingerprint(HashAlg::Sha256).to_string()))
}

/// Checks an armored sshsig signature of `msg` by `public_key` in spoko's namespace.
pub fn verify_signature(public_key: &str, msg: &[u8], signature: &str) -> Result<(), CryptoError> {
    let key = PublicKey::from_openssh(public_key.trim()).map_err(|_| CryptoError::Encoding)?;
    let sig = SshSig::from_pem(signature).map_err(|_| CryptoError::Signature)?;
    key.verify(NAMESPACE, msg, &sig).map_err(|_| CryptoError::Signature)
}

/// The first of `~/.ssh/id_ed25519`, `id_ecdsa`, `id_rsa` that exists.
pub fn default_key_path(home: &Path) -> Option<PathBuf> {
    DEFAULT_KEYS.iter().map(|n| home.join(".ssh").join(n)).find(|p| p.is_file())
}
