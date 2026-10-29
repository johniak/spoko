use std::collections::{HashMap, HashSet};
use std::io::Write;

use git2::{ObjectType, Oid};
use spoko_core::store::Store;
use spoko_crypto::{Keyring, ObjectId, git_object_id};

use super::ObjectServer;
use super::chunks::{Manifest, fetch_stream, parallel_get};
use super::codec::{KIND_MANIFEST, object_type};
use super::enumerate::{Role, deps};
use crate::{ClientError, Result};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DownloadStats {
    pub objects: usize,
    /// Server ids of everything downloaded (now known to be on the server).
    pub ids: Vec<ObjectId>,
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
    let odb = store.repo().odb()?;
    let mut stats = DownloadStats::default();
    let mut pending: HashMap<Oid, (ObjectType, Vec<u8>, Role)> = HashMap::new();
    let mut seen: HashSet<Oid> = HashSet::new();
    let mut frontier: Vec<(Oid, Role, bool)> = roots
        .iter()
        .filter(|o| !odb.exists(**o))
        .map(|o| (*o, Role::Snapshot, true))
        .collect();
    while !frontier.is_empty() {
        frontier.retain(|(o, _, _)| seen.insert(*o));
        let ids: Vec<ObjectId> = frontier
            .iter()
            .map(|(o, _, _)| git_object_id(kr, o.as_bytes()))
            .collect();
        let got: HashMap<ObjectId, (u8, Vec<u8>)> = parallel_get(server, kr, &ids)?
            .into_iter()
            .map(|(i, k, d)| (i, (k, d)))
            .collect();
        let mut next = Vec::new();
        for ((oid, role, required), id) in frontier.into_iter().zip(ids) {
            let Some((kind, data)) = got.get(&id) else {
                if required {
                    return Err(ClientError::Integrity(format!(
                        "object {oid} missing on server"
                    )));
                }
                continue;
            };
            let (kind, data) = if *kind == KIND_MANIFEST {
                let m = Manifest::from_bytes(data)?;
                let t = object_type(m.kind)?;
                if t == ObjectType::Blob {
                    let mut w = odb.writer(
                        usize::try_from(m.size)
                            .map_err(|_| ClientError::Integrity("too large".into()))?,
                        t,
                    )?;
                    fetch_stream(server, kr, &m, &mut w)?;
                    w.flush()?;
                    if w.finalize()? != oid {
                        return Err(ClientError::Integrity(format!(
                            "object {oid} content mismatch"
                        )));
                    }
                    stats.objects += 1;
                    stats.ids.push(id);
                    continue;
                }
                let mut buf = Vec::new();
                fetch_stream(server, kr, &m, &mut buf)?;
                (t, buf)
            } else {
                (object_type(*kind)?, data.clone())
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
                odb.write(kind, &data)?;
                continue;
            }
            for d in deps(kind, role, &data) {
                if !odb.exists(d.0) && !seen.contains(&d.0) {
                    next.push(d);
                }
            }
            pending.insert(oid, (kind, data, role));
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
            odb.write(*kind, data)?;
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
