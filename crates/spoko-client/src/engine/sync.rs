use std::collections::{BTreeMap, BTreeSet};

use git2::Oid;
use serde::{Deserialize, Serialize};
use spoko_core::gitstate::{gitstate_tree, merge_gitstate, read_gitstate};
use spoko_core::merge::{Conflict, MergeCtx, Side, carry_conflicts, merge_trees};
use spoko_core::platform::RelPath;
use spoko_core::snapshot::{
    SnapshotMeta, SnapshotParts, read_snapshot, same_content, write_snapshot,
};
use spoko_core::store::WorkMap;
use spoko_crypto::{name_id, open, seal};

use super::{Applied, Engine};
use crate::notifier::Notification;
use crate::transfer::download::fetch_closure;
use crate::transfer::enumerate::new_objects;
use crate::transfer::upload::push_objects;
use crate::{ClientError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOutcome {
    UpToDate,
    Pushed,
    FastForwarded,
    Merged { conflicts: usize },
    Paused,
}

const MAX_ROUNDS: usize = 10;
const ACTIVITY_EVERY_SECS: i64 = 60;

#[derive(Serialize, Deserialize)]
struct Activity {
    device_id: uuid::Uuid,
    time: i64,
}

pub(super) struct Merged {
    pub snapshot: Oid,
    pub new_conflicts: Vec<Conflict>,
}

impl Engine {
    fn head_name(&self) -> String {
        name_id(&self.ctx.keyring, "ptr", "head")
    }

    /// Current server head: (snapshot, pointer version).
    pub(super) fn read_head(&self) -> Result<Option<(Oid, u64)>> {
        let Some(p) = self
            .ctx
            .api
            .get_pointer(self.project_id, &self.head_name())?
        else {
            return Ok(None);
        };
        let plain = open(&self.ctx.keyring, b"head", &p.value)?;
        let oid = plain
            .get(..20)
            .and_then(|b| Oid::from_bytes(b).ok())
            .ok_or_else(|| ClientError::Integrity("bad head pointer".into()))?;
        Ok(Some((oid, p.version)))
    }

    fn objects(&self) -> crate::api::ProjectObjects {
        self.ctx.api.objects(self.project_id)
    }

    pub(super) fn fetch(&mut self, snapshot: Oid) -> Result<()> {
        let stats = fetch_closure(&self.store, &self.objects(), &self.ctx.keyring, &[snapshot])?;
        self.known.insert_many(stats.ids)?;
        Ok(())
    }

    fn upload(&mut self, snapshot: Oid) -> Result<()> {
        let objs = new_objects(&self.store, &[snapshot], &self.known, &self.ctx.keyring)?;
        let server = self.objects();
        push_objects(
            &self.store,
            &server,
            &self.ctx.keyring,
            &mut self.known,
            &objs,
        )?;
        Ok(())
    }

    /// Compare-and-swap of the head pointer; `Ok(false)` when another device won the race.
    fn cas_head(&mut self, expected: u64, snapshot: Oid) -> Result<bool> {
        let value = seal(
            &self.ctx.keyring,
            b"head",
            &[snapshot.as_bytes(), self.ctx.device_id.as_bytes()].concat(),
        );
        match self.ctx.api.put_pointer(
            self.project_id,
            &self.head_name(),
            Some(expected),
            &value,
        )? {
            Ok(p) => {
                self.state.head_version = p.version;
                Ok(true)
            }
            Err(_) => Ok(false),
        }
    }

    fn push_activity(&mut self) -> Result<()> {
        let now = self.now();
        if now - self.state.last_activity_push < ACTIVITY_EVERY_SECS {
            return Ok(());
        }
        let body = serde_json::to_vec(&Activity {
            device_id: self.ctx.device_id,
            time: now,
        })
        .map_err(|e| ClientError::Other(e.to_string()))?;
        let value = seal(&self.ctx.keyring, b"activity", &body);
        self.ctx
            .api
            .put_pointer(
                self.project_id,
                &name_id(&self.ctx.keyring, "ptr", "activity"),
                None,
                &value,
            )?
            .map_err(|_| ClientError::Other("activity pointer".into()))?;
        self.state.last_activity_push = now;
        Ok(())
    }

    pub fn sync(&mut self) -> Result<SyncOutcome> {
        self.sync_dirty(None)
    }

    /// One sync round. `dirty` (from the watcher) limits the scan; `None` scans everything.
    pub fn sync_dirty(&mut self, dirty: Option<&BTreeSet<RelPath>>) -> Result<SyncOutcome> {
        if self.state.paused.is_some() {
            return Ok(SyncOutcome::Paused);
        }
        let Some(mut local) = self.capture(dirty)? else {
            return Ok(SyncOutcome::Paused);
        };
        let mut merged: Option<usize> = None;
        for _ in 0..MAX_ROUNDS {
            let remote = self.read_head()?;
            let remote_oid = remote.map(|r| r.0);
            if remote_oid == self.state.base || remote.is_none() {
                if Some(local) == self.state.base && remote.is_some() {
                    return Ok(merged.map_or(SyncOutcome::UpToDate, |conflicts| {
                        SyncOutcome::Merged { conflicts }
                    }));
                }
                self.upload(local)?;
                if self.cas_head(remote.map_or(0, |r| r.1), local)? {
                    self.state.base = Some(local);
                    self.state.resolved.clear();
                    self.save()?;
                    // best effort: the sync itself already succeeded
                    let _ = self.push_activity();
                    return Ok(merged.map_or(SyncOutcome::Pushed, |conflicts| {
                        SyncOutcome::Merged { conflicts }
                    }));
                }
                continue;
            }
            let (remote, version) = remote.expect("checked");
            self.fetch(remote)?;
            if Some(local) == self.state.base {
                match self.apply_snapshot(remote, false)? {
                    Applied::LocalChanged => {
                        local = match self.capture(None)? {
                            Some(l) => l,
                            None => return Ok(SyncOutcome::Paused),
                        };
                        continue;
                    }
                    Applied::Done => {
                        self.state.base = Some(remote);
                        self.state.head_version = version;
                        self.save()?;
                        return Ok(merged.map_or(SyncOutcome::FastForwarded, |conflicts| {
                            SyncOutcome::Merged { conflicts }
                        }));
                    }
                }
            }
            let m = self.merge_snapshots(self.state.base, local, remote)?;
            let target = if same_content(&self.store, m.snapshot, remote)? {
                remote
            } else {
                m.snapshot
            };
            match self.apply_snapshot(target, false)? {
                Applied::LocalChanged => {
                    local = match self.capture(None)? {
                        Some(l) => l,
                        None => return Ok(SyncOutcome::Paused),
                    };
                    continue;
                }
                Applied::Done => {
                    for c in &m.new_conflicts {
                        self.notify(Notification::Conflict {
                            project: self.project_id,
                            path: c.path.clone(),
                            from_device: c.theirs_device.clone(),
                        });
                    }
                    merged = Some(merged.unwrap_or(0) + m.new_conflicts.len());
                    self.state.base = Some(remote);
                    self.state.head_version = version;
                    self.save()?;
                    local = target;
                }
            }
        }
        Err(ClientError::Other(format!(
            "sync did not converge after {MAX_ROUNDS} rounds"
        )))
    }

    /// Three-way merge of two snapshots, section by section. Commit parents: `[local, remote]`.
    pub(super) fn merge_snapshots(
        &mut self,
        base: Option<Oid>,
        local: Oid,
        remote: Oid,
    ) -> Result<Merged> {
        let store = &self.store;
        let b = base.map(|o| read_snapshot(store, o)).transpose()?;
        let l = read_snapshot(store, local)?;
        let r = read_snapshot(store, remote)?;
        let empty = store.build_tree(&WorkMap::default())?;
        let ours_newer = l.meta.time >= r.meta.time;
        let now = self.now();
        let matcher = &self.matcher;
        let work_binary = |p: &RelPath, head: &[u8], size: u64| matcher.is_binary(p, head, size);
        let nul_binary = |_: &RelPath, head: &[u8], _: u64| head.iter().take(8000).any(|b| *b == 0);
        let ctx = MergeCtx {
            prefix: "work/",
            ours_newer,
            is_binary: &work_binary,
            union_jsonl: false,
            theirs_device: r.meta.device_name.clone(),
            now,
        };
        let work = merge_trees(
            store,
            b.as_ref().map_or(empty, |b| b.work),
            l.work,
            r.work,
            &ctx,
        )?;
        let mut new_conflicts = work.new_conflicts;
        let mut overwrites = work.binary_overwrites;

        let names: BTreeSet<&String> = l.externals.keys().chain(r.externals.keys()).collect();
        let mut externals = BTreeMap::new();
        for n in names {
            let pick = |m: &BTreeMap<String, Oid>| m.get(n).copied().unwrap_or(empty);
            let prefix = format!("external/{n}/");
            let ectx = MergeCtx {
                prefix: &prefix,
                ours_newer,
                is_binary: &nul_binary,
                union_jsonl: true,
                theirs_device: r.meta.device_name.clone(),
                now,
            };
            let m = merge_trees(
                store,
                b.as_ref().map_or(empty, |b| pick(&b.externals)),
                pick(&l.externals),
                pick(&r.externals),
                &ectx,
            )?;
            new_conflicts.extend(m.new_conflicts);
            overwrites.extend(m.binary_overwrites);
            if m.tree != empty {
                externals.insert(n.clone(), m.tree);
            }
        }

        let gitstate = match (l.gitstate, r.gitstate) {
            (x, None) => x,
            (None, y) => y,
            (Some(x), Some(y)) if x == y => Some(x),
            (Some(x), Some(y)) => {
                let base_gs = b
                    .as_ref()
                    .and_then(|b| b.gitstate)
                    .map(|t| read_gitstate(store, t))
                    .transpose()?
                    .flatten();
                match (read_gitstate(store, x)?, read_gitstate(store, y)?) {
                    (Some(lg), Some(rg)) => {
                        let older = if ours_newer {
                            &r.meta.device_name
                        } else {
                            &l.meta.device_name
                        };
                        Some(gitstate_tree(
                            store,
                            &merge_gitstate(base_gs.as_ref(), &lg, &rg, ours_newer, older).state,
                        )?)
                    }
                    _ => Some(if ours_newer { x } else { y }),
                }
            }
        };

        let mut conflicts =
            carry_conflicts(&l.meta.conflicts, &r.meta.conflicts, &self.state.resolved);
        // a conflict both had at the base that one side dropped was resolved there
        if let Some(b) = &b {
            conflicts.retain(|c| !b.meta.conflicts.contains(c) || (l.meta.conflicts.contains(c) && r.meta.conflicts.contains(c)));
        }
        for c in &new_conflicts {
            conflicts.retain(|x| x.path != c.path);
            conflicts.push(c.clone());
        }
        let parts = SnapshotParts {
            work: work.tree,
            gitstate,
            externals,
            meta: SnapshotMeta {
                device_id: self.ctx.device_id,
                device_name: self.ctx.device_name.clone(),
                time: now,
                conflicts,
            },
        };
        let snapshot = write_snapshot(store, &parts, &[local, remote])?;
        for (path, side) in overwrites {
            if side == Side::Ours {
                self.notify(Notification::BinaryOverwritten {
                    project: self.project_id,
                    path,
                    from_device: r.meta.device_name.clone(),
                });
            }
        }
        Ok(Merged {
            snapshot,
            new_conflicts,
        })
    }
}
