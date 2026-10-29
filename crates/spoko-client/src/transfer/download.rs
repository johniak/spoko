use std::collections::{HashMap, HashSet};
use std::io::Write;

use git2::{ObjectType, Oid};
use spoko_core::store::Store;
use spoko_crypto::{Keyring, ObjectId, git_object_id};

use super::ObjectServer;
use super::chunks::{Buffered, GET_BYTES, Manifest, fetch_stream, parallel_get_each};
use super::codec::{KIND_MANIFEST, object_type};
use super::enumerate::{Role, deps};
use crate::{ClientError, Result};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DownloadStats {
    pub objects: usize,
    /// Server ids of everything downloaded (now known to be on the server).
    pub ids: Vec<ObjectId>,
    /// Most decrypted objects held at once on their way to the store (trees, commits and tags
    /// waiting for the final write not counted).
    pub peak_buffered: Buffered,
}

fn verify(kind: ObjectType, data: &[u8], oid: Oid) -> Result<()> {
    if Oid::hash_object(kind, data)? == oid {
        Ok(())
    } else {
        Err(ClientError::Integrity(format!(
            "object {oid} content mismatch"
        )))
    }
}

/// Downloads everything reachable from snapshot `roots` that the store lacks.
///
/// Blobs are written as they arrive (they have no dependencies). Trees, commits and tags are
/// written only after the whole closure is present, children first, so an interrupted download
/// never leaves a tree in the store whose blobs are missing.
pub fn fetch_closure(
    store: &Store,
    server: &dyn ObjectServer,
    kr: &Keyring,
    roots: &[Oid],
) -> Result<DownloadStats> {
    fetch_closure_limited(store, server, kr, roots, GET_BYTES)
}

/// [`fetch_closure`] with download requests of about `request_bytes` (what bounds the memory
/// a wide level of the walk takes, see [`parallel_get_each`]).
pub fn fetch_closure_limited(
    store: &Store,
    server: &dyn ObjectServer,
    kr: &Keyring,
    roots: &[Oid],
    request_bytes: usize,
) -> Result<DownloadStats> {
    // small objects are collected into packfiles; big blobs stream to loose files
    store.batch(|| fetch_closure_inner(store, server, kr, roots, request_bytes))
}

fn fetch_closure_inner(
    store: &Store,
    server: &dyn ObjectServer,
    kr: &Keyring,
    roots: &[Oid],
    request_bytes: usize,
) -> Result<DownloadStats> {
    let odb = store.repo().odb()?;
    let mut stats = DownloadStats::default();
    let mut pending: HashMap<Oid, (ObjectType, Vec<u8>, Role)> = HashMap::new();
    let mut seen: HashSet<Oid> = HashSet::new();
    let mut frontier: Vec<(Oid, Role, bool)> = roots
        .iter()
        .filter(|o| !store.has(**o))
        .map(|o| (*o, Role::Snapshot, true))
        .collect();
    while !frontier.is_empty() {
        frontier.retain(|(o, _, _)| seen.insert(*o));
        let ids: Vec<ObjectId> = frontier
            .iter()
            .map(|(o, _, _)| git_object_id(kr, o.as_bytes()))
            .collect();
        // objects of this level not received yet
        let mut wanted: HashMap<ObjectId, (Oid, Role, bool)> = ids.iter().copied().zip(frontier).collect();
        let mut next = Vec::new();
        let mut take = |id: ObjectId, kind: u8, data: Vec<u8>| -> Result<()> {
            let Some((oid, role, _)) = wanted.remove(&id) else { return Ok(()) };
            let (kind, data) = if kind == KIND_MANIFEST {
                let m = Manifest::from_bytes(&data)?;
                let t = object_type(m.kind)?;
                if t == ObjectType::Blob {
                    let size = usize::try_from(m.size).map_err(|_| ClientError::Integrity("too large".into()))?;
                    let mut w = odb.writer(size, t)?;
                    fetch_stream(server, kr, &m, &mut w)?;
                    w.flush()?;
                    if w.finalize()? != oid {
                        return Err(ClientError::Integrity(format!(
                            "object {oid} content mismatch"
                        )));
                    }
                    stats.objects += 1;
                    stats.ids.push(id);
                    return Ok(());
                }
                let mut buf = Vec::new();
                fetch_stream(server, kr, &m, &mut buf)?;
                (t, buf)
            } else {
                (object_type(kind)?, data)
            };
            verify(kind, &data, oid)?;
            stats.objects += 1;
            stats.ids.push(id);
            let role = if role == Role::Any && kind == ObjectType::Commit {
                Role::UserCommit
            } else {
                role
            };
            if kind == ObjectType::Blob {
                store.write_raw(kind, &data)?;
                return Ok(());
            }
            for d in deps(kind, role, &data) {
                if !store.has(d.0) && !seen.contains(&d.0) {
                    next.push(d);
                }
            }
            pending.insert(oid, (kind, data, role));
            Ok(())
        };
        let held = parallel_get_each(server, kr, &ids, request_bytes, &mut take)?;
        stats.peak_buffered.bytes = stats.peak_buffered.bytes.max(held.bytes);
        stats.peak_buffered.objects = stats.peak_buffered.objects.max(held.objects);
        if let Some((oid, _, _)) = wanted.values().find(|(_, _, required)| *required) {
            return Err(ClientError::Integrity(format!(
                "object {oid} missing on server"
            )));
        }
        frontier = next;
    }
    // Post-order write of trees/commits/tags.
    let mut stack: Vec<(Oid, bool)> = roots.iter().map(|o| (*o, false)).collect();
    while let Some((oid, expanded)) = stack.pop() {
        let Some((kind, data, role)) = pending.get(&oid) else {
            continue;
        };
        if expanded {
            store.write_raw(*kind, data)?;
            pending.remove(&oid);
            continue;
        }
        stack.push((oid, true));
        for (d, _, _) in deps(*kind, *role, data) {
            if pending.contains_key(&d) {
                stack.push((d, false));
            }
        }
    }
    Ok(stats)
}
