//! Local snapshot history and restoring single files from it.

use std::collections::{BinaryHeap, HashSet};
use std::path::Path;

use git2::Oid;
use spoko_core::platform::{self, RelPath};
use spoko_core::snapshot::read_snapshot;

use crate::engine::Engine;
use crate::{ClientError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub commit: Oid,
    pub time: i64,
    pub device_name: String,
    /// The snapshot changed the requested path (always true without a path).
    pub changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestoreFrom {
    Commit(Oid),
    /// The newest snapshot taken at or before this time (unix seconds).
    Before(i64),
    /// The snapshot of this machine's files taken when it joined the project.
    Join,
}

/// `(oid, filemode)` of `path` inside a snapshot's `work/` tree.
fn entry_at(e: &Engine, snapshot: Oid, path: &RelPath) -> Result<Option<(Oid, i32)>> {
    let work = read_snapshot(&e.store, snapshot)?.work;
    let tree = e.store.repo().find_tree(work)?;
    Ok(tree.get_path(Path::new(path.as_str())).ok().map(|t| (t.id(), t.filemode())))
}

/// Snapshots reachable from the current one, newest first. Only locally available snapshots
/// are listed: the walk stops where a parent was never downloaded.
pub fn log(e: &Engine, path: Option<&RelPath>, limit: usize) -> Result<Vec<LogEntry>> {
    let repo = e.store.repo();
    let mut out = Vec::new();
    let mut heap: BinaryHeap<(i64, Oid)> = BinaryHeap::new();
    let mut seen: HashSet<Oid> = HashSet::new();
    if let Some(start) = e.state.last_local.or(e.state.base) {
        heap.push((read_snapshot(&e.store, start)?.meta.time, start));
        seen.insert(start);
    }
    while let Some((time, oid)) = heap.pop() {
        if out.len() >= limit {
            break;
        }
        let meta = read_snapshot(&e.store, oid)?.meta;
        let parents: Vec<Oid> = repo.find_commit(oid)?.parent_ids().filter(|p| e.store.has(*p)).collect();
        let changed = match path {
            None => true,
            Some(p) => {
                let mine = entry_at(e, oid, p)?;
                let before = match parents.first() {
                    Some(parent) => entry_at(e, *parent, p)?,
                    None => None,
                };
                mine != before
            }
        };
        out.push(LogEntry { commit: oid, time, device_name: meta.device_name, changed });
        for p in parents {
            if seen.insert(p) {
                heap.push((read_snapshot(&e.store, p)?.meta.time, p));
            }
        }
    }
    Ok(out)
}

/// Writes `path` as it was in an older snapshot. It becomes an ordinary local change.
pub fn restore(e: &mut Engine, path: &RelPath, from: RestoreFrom) -> Result<()> {
    let snapshot = match from {
        RestoreFrom::Commit(o) => o,
        RestoreFrom::Join => e.state.join_snapshot.ok_or_else(|| ClientError::Other("this machine has no join snapshot".into()))?,
        RestoreFrom::Before(t) => {
            log(e, None, usize::MAX)?.into_iter().find(|x| x.time <= t).ok_or_else(|| ClientError::Other(format!("no local snapshot at or before {t}")))?.commit
        }
    };
    let abs = path.to_fs(&e.root);
    match entry_at(e, snapshot, path)? {
        Some((oid, 0o120000)) => {
            let target = String::from_utf8_lossy(&e.store.read_blob(oid)?).into_owned();
            platform::write_symlink(&abs, &target)?;
        }
        Some((oid, mode)) => {
            let store = &e.store;
            platform::atomic_write_with(&abs, mode == 0o100755, &mut |w| store.copy_blob_to(oid, w).map(|_| ()).map_err(std::io::Error::other))?;
        }
        None => match std::fs::remove_file(&abs) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err.into()),
            _ => {}
        },
    }
    Ok(())
}
