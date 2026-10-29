use std::collections::HashSet;

use git2::{ObjectType, Oid};
use spoko_core::store::Store;
use spoko_crypto::{Keyring, ObjectId, git_object_id};

use super::chunks::{CHUNK_THRESHOLD, Manifest, parallel_put, push_stream};
use super::codec::{KIND_MANIFEST, kind_of};
use super::known::KnownSet;
use super::{FLUSH_BYTES, FLUSH_OBJECTS, ObjectServer};
use crate::Result;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UploadStats {
    pub objects: usize,
    pub chunks: usize,
    pub bytes: usize,
}

/// Uploads the objects the server lacks and records all of them as known.
pub fn push_objects(
    store: &Store,
    server: &dyn ObjectServer,
    kr: &Keyring,
    known: &mut KnownSet,
    objs: &[(Oid, ObjectType)],
) -> Result<UploadStats> {
    let mut stats = UploadStats::default();
    let ids: Vec<ObjectId> = objs
        .iter()
        .map(|(o, _)| git_object_id(kr, o.as_bytes()))
        .collect();
    let mut missing: HashSet<ObjectId> = HashSet::new();
    for part in ids.chunks(1000) {
        missing.extend(server.missing(part)?);
    }
    let odb = store.repo().odb()?;
    let mut items: Vec<(ObjectId, u8, Vec<u8>)> = Vec::new();
    let mut bytes = 0;
    for ((oid, kind), id) in objs.iter().zip(&ids) {
        if !missing.contains(id) {
            continue;
        }
        let (size, _) = odb.read_header(*oid)?;
        let item = if size as u64 > CHUNK_THRESHOLD {
            let (reader, _, _) = odb.reader(*oid)?;
            let (manifest, uploaded): (Manifest, usize) =
                push_stream(server, kr, reader, kind_of(*kind))?;
            stats.chunks += uploaded;
            (*id, KIND_MANIFEST, manifest.to_bytes())
        } else {
            let obj = odb.read(*oid)?;
            (*id, kind_of(*kind), obj.data().to_vec())
        };
        bytes += item.2.len();
        items.push(item);
        stats.objects += 1;
        if bytes >= FLUSH_BYTES || items.len() >= FLUSH_OBJECTS {
            stats.bytes += parallel_put(server, kr, std::mem::take(&mut items))?;
            bytes = 0;
        }
    }
    stats.bytes += parallel_put(server, kr, items)?;
    known.insert_many(ids)?;
    Ok(stats)
}
