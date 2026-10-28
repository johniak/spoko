use crypto_box::{PublicKey, SecretKey};

use crate::CryptoError;

/// Long-lived X25519 identity of one device; the keyring is delivered sealed to it.
pub struct DeviceKeypair {
    secret: SecretKey,
}

impl DeviceKeypair {
    pub fn generate() -> DeviceKeypair {
        DeviceKeypair { secret: SecretKey::generate(&mut rand_core::OsRng) }
    }

    pub fn from_secret_bytes(bytes: [u8; 32]) -> DeviceKeypair {
        DeviceKeypair { secret: SecretKey::from_bytes(bytes) }
    }

    pub fn secret_bytes(&self) -> [u8; 32] {
        self.secret.to_bytes()
    }

    pub fn public(&self) -> [u8; 32] {
        self.secret.public_key().to_bytes()
    }

    pub fn unseal(&self, sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.secret.unseal(sealed).map_err(|_| CryptoError::Decrypt)
    }
}

pub fn seal_to(pubkey: &[u8; 32], data: &[u8]) -> Vec<u8> {
    PublicKey::from_bytes(*pubkey)
        .seal(&mut rand_core::OsRng, data)
        .expect("sealing cannot fail for in-memory buffers")
}

/// Short code both devices display during pairing, so the user can spot a swapped key.
pub fn fingerprint(pubkey: &[u8; 32]) -> String {
    let hex = data_encoding::HEXLOWER.encode(&blake3::hash(pubkey).as_bytes()[..6]);
    format!("{}-{}-{}", &hex[0..4], &hex[4..8], &hex[8..12])
}
