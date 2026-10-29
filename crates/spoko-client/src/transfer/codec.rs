//! Plaintext framing `[kind u8][flags u8][payload]`, flags bit0 = zstd.

use git2::ObjectType;
use spoko_crypto::{Keyring, ObjectId, open, seal};

use crate::{ClientError, Result};

pub const KIND_COMMIT: u8 = 1;
pub const KIND_TREE: u8 = 2;
pub const KIND_BLOB: u8 = 3;
pub const KIND_TAG: u8 = 4;
pub const KIND_MANIFEST: u8 = 5;
pub const KIND_CHUNK: u8 = 6;

const ZSTD_LEVEL: i32 = 3;

pub fn kind_of(t: ObjectType) -> u8 {
    match t {
        ObjectType::Commit => KIND_COMMIT,
        ObjectType::Tree => KIND_TREE,
        ObjectType::Tag => KIND_TAG,
        _ => KIND_BLOB,
    }
}

pub fn object_type(kind: u8) -> Result<ObjectType> {
    match kind {
        KIND_COMMIT => Ok(ObjectType::Commit),
        KIND_TREE => Ok(ObjectType::Tree),
        KIND_BLOB => Ok(ObjectType::Blob),
        KIND_TAG => Ok(ObjectType::Tag),
        k => Err(ClientError::Integrity(format!("not a git object kind: {k}"))),
    }
}

pub fn encode(kind: u8, data: &[u8]) -> Vec<u8> {
    if let Ok(z) = zstd::bulk::compress(data, ZSTD_LEVEL)
        && z.len() * 100 < data.len() * 95
    {
        return [&[kind, 1][..], &z].concat();
    }
    [&[kind, 0][..], data].concat()
}

pub fn decode(plain: &[u8]) -> Result<(u8, Vec<u8>)> {
    let [kind, flags, payload @ ..] = plain else { return Err(ClientError::Integrity("short object".into())) };
    let data = if flags & 1 == 1 { zstd::stream::decode_all(payload).map_err(|e| ClientError::Integrity(e.to_string()))? } else { payload.to_vec() };
    Ok((*kind, data))
}

pub fn seal_object(kr: &Keyring, id: &ObjectId, kind: u8, data: &[u8]) -> Vec<u8> {
    seal(kr, &id.0, &encode(kind, data))
}

pub fn open_object(kr: &Keyring, id: &ObjectId, sealed: &[u8]) -> Result<(u8, Vec<u8>)> {
    decode(&open(kr, &id.0, sealed)?)
}
