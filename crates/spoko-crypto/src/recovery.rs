use zeroize::Zeroize;

use crate::aead::{open_raw, seal_raw};
use crate::{CryptoError, random_bytes};

const AAD: &[u8] = b"spoko-recovery";

/// Printed once at account creation; unwraps the keyring when no device is left.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RecoveryKey([u8; 32]);

impl Drop for RecoveryKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl RecoveryKey {
    pub fn generate() -> RecoveryKey {
        RecoveryKey(random_bytes())
    }

    pub fn to_display(&self) -> String {
        let s = data_encoding::BASE32_NOPAD.encode(&self.0);
        s.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).expect("ascii")).collect::<Vec<_>>().join("-")
    }

    pub fn parse(s: &str) -> Result<RecoveryKey, CryptoError> {
        let clean: String = s.chars().filter(|c| !c.is_whitespace() && *c != '-').collect::<String>().to_uppercase();
        let bytes = data_encoding::BASE32_NOPAD.decode(clean.as_bytes()).map_err(|_| CryptoError::Encoding)?;
        Ok(RecoveryKey(bytes.try_into().map_err(|_| CryptoError::Encoding)?))
    }

    pub fn wrap(&self, data: &[u8]) -> Vec<u8> {
        seal_raw(&self.0, AAD, data)
    }

    pub fn unwrap(&self, wrapped: &[u8]) -> Result<Vec<u8>, CryptoError> {
        open_raw(&self.0, AAD, wrapped)
    }
}
