use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::{CryptoError, random_bytes};

pub type Key32 = [u8; 32];

/// All data keys of an account (old versions stay readable after rotation) plus the
/// key used to derive opaque object ids. `id_key` never rotates so ids stay stable.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keyring {
    pub keys: BTreeMap<u32, Key32>,
    pub id_key: Key32,
}

impl std::fmt::Debug for Keyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Keyring(versions: {:?})", self.keys.keys().collect::<Vec<_>>())
    }
}

impl Drop for Keyring {
    fn drop(&mut self) {
        for k in self.keys.values_mut() {
            k.zeroize();
        }
        self.id_key.zeroize();
    }
}

impl Keyring {
    pub fn generate() -> Keyring {
        Keyring { keys: BTreeMap::from([(1, random_bytes())]), id_key: random_bytes() }
    }

    pub fn current(&self) -> (u32, &Key32) {
        let (v, k) = self.keys.last_key_value().expect("keyring is never empty");
        (*v, k)
    }

    /// Adds a new data key version and returns it.
    pub fn rotate(&mut self) -> u32 {
        let next = self.current().0 + 1;
        self.keys.insert(next, random_bytes());
        next
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).expect("keyring serializes")
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Keyring, CryptoError> {
        let kr: Keyring = bincode::deserialize(bytes).map_err(|_| CryptoError::Encoding)?;
        if kr.keys.is_empty() {
            return Err(CryptoError::Encoding);
        }
        Ok(kr)
    }
}
