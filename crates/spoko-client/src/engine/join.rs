use std::path::Path;

use git2::Oid;
use spoko_core::platform::{DiskKind, RelPath, read_entry};
use spoko_core::snapshot::{read_snapshot, write_snapshot};
use spoko_core::store::{Entry, EntryMode};
use uuid::Uuid;

use super::{Applied, DeviceCtx, Engine};
use crate::{ClientError, Result};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JoinReport {
    /// Paths where this machine's previous content differed from the project (kept in the join snapshot).
    pub differing: Vec<RelPath>,
    pub join_snapshot: Option<Oid>,
}

impl Engine {
    /// Connects a directory to an existing project. An empty directory gets a plain checkout.
    /// A non-empty one (an existing clone) is captured first as a parentless *join snapshot*,
    /// then the project's state replaces it; local branches the project lacks are kept.
    pub fn clone_into(
        ctx: DeviceCtx,
        project_id: Uuid,
        root: &Path,
    ) -> Result<(Engine, JoinReport)> {
        std::fs::create_dir_all(root)?;
        let empty = std::fs::read_dir(root)?.next().is_none();
        let mut e = Engine::open(ctx, project_id, root)?;
        let (remote, version) = e
            .read_head()?
            .ok_or_else(|| ClientError::Other("project has no snapshot yet".into()))?;
        e.fetch(remote)?;
        let mut report = JoinReport::default();
        if !empty {
            e.state.last_local = None;
            e.work.clear();
            e.ext_work.clear();
            let local = e
                .capture(None)?
                .ok_or_else(|| ClientError::Other("project directory vanished".into()))?;
            let local = e.with_shadowed_files(local, remote)?;
            let (lw, rw) = (
                e.work.clone(),
                e.store.read_tree(read_snapshot(&e.store, remote)?.work)?,
            );
            report.differing = lw
                .keys()
                .chain(rw.keys())
                .filter(|p| lw.get(*p) != rw.get(*p))
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            report.join_snapshot = Some(local);
            e.state.join_snapshot = Some(local);
        }
        match e.apply_snapshot(remote, !empty)? {
            Applied::Done => {}
            Applied::LocalChanged => {
                return Err(ClientError::Other(
                    "files changed during join; run it again".into(),
                ));
            }
        }
        e.state.base = Some(remote);
        e.state.head_version = version;
        e.save()?;
        Ok((e, report))
    }

    /// Files on disk at paths the project tracks but this machine ignores (e.g. `.env` before
    /// `.spokoconfig` arrived) go into the join snapshot too, so overwriting them loses nothing.
    fn with_shadowed_files(&mut self, local: Oid, remote: Oid) -> Result<Oid> {
        let remote_work = self
            .store
            .read_tree(read_snapshot(&self.store, remote)?.work)?;
        let mut extra = Vec::new();
        for (p, re) in remote_work.iter() {
            if self.work.contains_key(p) || re.mode == EntryMode::Commit {
                continue;
            }
            let abs = p.to_fs(&self.root);
            if read_entry(&abs)?.is_some_and(|d| d.kind == DiskKind::File) {
                let oid = self.store.write_blob(&std::fs::read(&abs)?)?;
                extra.push((
                    p.clone(),
                    Entry {
                        oid,
                        mode: EntryMode::Blob,
                    },
                ));
            }
        }
        if extra.is_empty() {
            return Ok(local);
        }
        self.work.extend(extra);
        let mut parts = read_snapshot(&self.store, local)?;
        parts.work = self.store.build_tree(&self.work)?;
        let oid = write_snapshot(&self.store, &parts, &[])?;
        self.state.last_local = Some(oid);
        Ok(oid)
    }
}
