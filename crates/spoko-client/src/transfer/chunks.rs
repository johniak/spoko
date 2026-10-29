//! Content-defined chunking for big objects and Docker volume streams.

use std::io::{Read, Write};

use fastcdc::v2020::StreamCDC;
use serde::{Deserialize, Serialize};
use spoko_crypto::{Keyring, ObjectId, chunk_id};

use super::codec::{KIND_CHUNK, open_object, seal_object};
use super::{FLUSH_BYTES, ObjectServer, WORKERS};
use crate::{ClientError, Result};

pub const CHUNK_THRESHOLD: u64 = 1 << 20;
pub const CHUNK_MIN: usize = 256 << 10;
pub const CHUNK_AVG: usize = 1 << 20;
pub const CHUNK_MAX: usize = 4 << 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Kind of the reassembled object (codec kind; volumes use KIND_BLOB).
    pub kind: u8,
    pub size: u64,
    pub chunks: Vec<ObjectId>,
}

impl Manifest {
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).expect("manifest serializes")
    }
    pub fn from_bytes(b: &[u8]) -> Result<Manifest> {
        bincode::deserialize(b).map_err(|e| ClientError::Integrity(format!("bad manifest: {e}")))
    }
}

/// Seals and uploads `items` (id, kind, plaintext) using up to [`WORKERS`] threads.
pub fn parallel_put(
    server: &dyn ObjectServer,
    kr: &Keyring,
    items: Vec<(ObjectId, u8, Vec<u8>)>,
) -> Result<usize> {
    if items.is_empty() {
        return Ok(0);
    }
    let mut groups: Vec<Vec<(ObjectId, u8, Vec<u8>)>> = (0..WORKERS).map(|_| Vec::new()).collect();
    let mut sizes = [0usize; WORKERS];
    for it in items {
        let i = (0..WORKERS).min_by_key(|i| sizes[*i]).expect("workers > 0");
        sizes[i] += it.2.len();
        groups[i].push(it);
    }
    std::thread::scope(|s| {
        let handles: Vec<_> = groups
            .into_iter()
            .filter(|g| !g.is_empty())
            .map(|g| {
                s.spawn(move || -> Result<usize> {
                    let mut sent = 0;
                    let mut batch: Vec<(ObjectId, Vec<u8>)> = Vec::new();
                    let mut bytes = 0;
                    for (id, kind, data) in g {
                        let sealed = seal_object(kr, &id, kind, &data);
                        if bytes + sealed.len() + 36 > spoko_proto::MAX_BATCH_BYTES
                            && !batch.is_empty()
                        {
                            server.put(&batch)?;
                            batch.clear();
                            bytes = 0;
                        }
                        bytes += sealed.len() + 36;
                        sent += sealed.len();
                        batch.push((id, sealed));
                    }
                    // groups are never empty, so neither is the last batch
                    server.put(&batch)?;
                    Ok(sent)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("upload worker panicked"))
            .sum()
    })
}

/// Fetches and decrypts `ids` in parallel; missing ids are simply absent from the result.
pub fn parallel_get(
    server: &dyn ObjectServer,
    kr: &Keyring,
    ids: &[ObjectId],
) -> Result<Vec<(ObjectId, u8, Vec<u8>)>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let per = ids.len().div_ceil(WORKERS).max(1);
    std::thread::scope(|s| {
        let handles: Vec<_> = ids
            .chunks(per)
            .map(|part| {
                s.spawn(move || -> Result<Vec<(ObjectId, u8, Vec<u8>)>> {
                    let mut out = Vec::new();
                    for sub in part.chunks(256) {
                        for (id, sealed) in server.get(sub)? {
                            let (kind, data) = open_object(kr, &id, &sealed)?;
                            out.push((id, kind, data));
                        }
                    }
                    Ok(out)
                })
            })
            .collect();
        let mut all = Vec::new();
        for h in handles {
            all.extend(h.join().expect("download worker panicked")?);
        }
        Ok(all)
    })
}

fn flush(
    server: &dyn ObjectServer,
    kr: &Keyring,
    pending: &mut Vec<(ObjectId, Vec<u8>)>,
    stats: &mut usize,
) -> Result<()> {
    let ids: Vec<ObjectId> = pending.iter().map(|(i, _)| *i).collect();
    let missing: std::collections::HashSet<ObjectId> = server.missing(&ids)?.into_iter().collect();
    let items: Vec<_> = pending
        .drain(..)
        .filter(|(id, _)| missing.contains(id))
        .map(|(id, d)| (id, KIND_CHUNK, d))
        .collect();
    *stats += items.len();
    parallel_put(server, kr, items)?;
    Ok(())
}

/// Chunks a stream, uploads chunks the server lacks, returns the manifest.
pub fn push_stream(
    server: &dyn ObjectServer,
    kr: &Keyring,
    reader: impl Read,
    kind: u8,
) -> Result<(Manifest, usize)> {
    let mut chunks = Vec::new();
    let mut size = 0;
    let mut pending: Vec<(ObjectId, Vec<u8>)> = Vec::new();
    let mut pending_bytes = 0;
    let mut uploaded = 0;
    for c in StreamCDC::new(reader, CHUNK_MIN, CHUNK_AVG, CHUNK_MAX) {
        let c = c.map_err(|e| ClientError::Io(std::io::Error::other(e.to_string())))?;
        let id = chunk_id(kr, &c.data);
        size += c.length as u64;
        chunks.push(id);
        pending_bytes += c.data.len();
        pending.push((id, c.data));
        if pending_bytes >= FLUSH_BYTES {
            flush(server, kr, &mut pending, &mut uploaded)?;
            pending_bytes = 0;
        }
    }
    flush(server, kr, &mut pending, &mut uploaded)?;
    Ok((Manifest { kind, size, chunks }, uploaded))
}

/// Downloads the chunks of `manifest` in order and writes them to `out`.
pub fn fetch_stream(
    server: &dyn ObjectServer,
    kr: &Keyring,
    manifest: &Manifest,
    out: &mut dyn Write,
) -> Result<()> {
    for window in manifest.chunks.chunks(WORKERS * 4) {
        let mut unique: Vec<ObjectId> = window.to_vec();
        unique.sort();
        unique.dedup();
        let got: std::collections::HashMap<ObjectId, Vec<u8>> = parallel_get(server, kr, &unique)?
            .into_iter()
            .map(|(i, _, d)| (i, d))
            .collect();
        for id in window {
            let data = got
                .get(id)
                .ok_or_else(|| ClientError::Integrity(format!("missing chunk {id}")))?;
            if chunk_id(kr, data) != *id {
                return Err(ClientError::Integrity(format!(
                    "chunk {id} content mismatch"
                )));
            }
            out.write_all(data)?;
        }
    }
    Ok(())
}
