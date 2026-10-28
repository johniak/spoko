use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{CryptoError, Keyring};

/// Opaque id of a stored object. Derived with a keyed hash so the server cannot
/// test whether a given git object or file content exists.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId(pub [u8; 32]);

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&data_encoding::HEXLOWER.encode(&self.0))
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({self})")
    }
}

impl FromStr for ObjectId {
    type Err = CryptoError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = data_encoding::HEXLOWER_PERMISSIVE.decode(s.as_bytes()).map_err(|_| CryptoError::Encoding)?;
        Ok(ObjectId(bytes.try_into().map_err(|_| CryptoError::Encoding)?))
    }
}

impl Serialize for ObjectId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ObjectId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

fn keyed(kr: &Keyring, parts: &[&[u8]]) -> [u8; 32] {
    let mut h = blake3::Hasher::new_keyed(&kr.id_key);
    for p in parts {
        h.update(p);
    }
    *h.finalize().as_bytes()
}

pub fn git_object_id(kr: &Keyring, oid: &[u8]) -> ObjectId {
    ObjectId(keyed(kr, &[b"git\0", oid]))
}

pub fn chunk_id(kr: &Keyring, chunk: &[u8]) -> ObjectId {
    ObjectId(keyed(kr, &[b"chunk\0", blake3::hash(chunk).as_bytes()]))
}

/// Opaque name for server-side pointers (`kind` separates namespaces).
pub fn name_id(kr: &Keyring, kind: &str, name: &str) -> String {
    data_encoding::HEXLOWER.encode(&keyed(kr, &[b"name\0", kind.as_bytes(), b"\0", name.as_bytes()]))
}
