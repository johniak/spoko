//! Moving objects between the internal store and the server, encrypted.
//!
//! Every object is compressed (zstd, skipped when it does not help), then sealed with the
//! account keyring under an opaque keyed id. Objects over [`chunks::CHUNK_THRESHOLD`] are split
//! with FastCDC so a small edit to a big file uploads only the changed chunks.

pub mod chunks;
pub mod codec;
pub mod download;
pub mod enumerate;
pub mod known;
pub mod upload;

use spoko_crypto::ObjectId;

use crate::Result;

/// The part of the server API transfer needs (implemented by the HTTP client and test fakes).
pub trait ObjectServer: Send + Sync {
    fn missing(&self, ids: &[ObjectId]) -> Result<Vec<ObjectId>>;
    fn put(&self, batch: &[(ObjectId, Vec<u8>)]) -> Result<()>;
    fn get(&self, ids: &[ObjectId]) -> Result<Vec<(ObjectId, Vec<u8>)>>;
}

/// Parallelism of uploads/downloads.
pub const WORKERS: usize = 4;
/// Plaintext bytes gathered before a parallel flush.
pub const FLUSH_BYTES: usize = 8 << 20;
/// Also flush after this many objects, so many tiny files still go up in parallel batches.
pub const FLUSH_OBJECTS: usize = 4096;
