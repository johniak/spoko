use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use git2::Oid;
use serde::{Deserialize, Serialize};
use spoko_core::guard::DeleteLog;
use spoko_core::platform::RelPath;
use spoko_core::store::Entry;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PauseReason {
    User,
    MassDelete { deleted: usize, total: usize },
    MissingRoot,
}

/// Per-project sync state, persisted as `state.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectState {
    /// Last snapshot that is both on the server and fully applied here (merge base).
    #[serde(with = "spoko_core::serde_oid::opt", default)]
    pub base: Option<Oid>,
    pub head_version: u64,
    /// Last local snapshot (parent of the next capture).
    #[serde(with = "spoko_core::serde_oid::opt", default)]
    pub last_local: Option<Oid>,
    pub paused: Option<PauseReason>,
    /// Skip the mass-delete guard once (`spoko resume --confirm`).
    pub guard_override: bool,
    /// Recent local deletions, judged together by the mass-delete guard.
    pub deletions: DeleteLog,
    pub pending_hooks: Vec<usize>,
    #[serde(with = "spoko_core::serde_oid::opt", default)]
    pub join_snapshot: Option<Oid>,
    /// Remote gitstate tree not yet applied because git was busy.
    #[serde(with = "spoko_core::serde_oid::opt", default)]
    pub pending_gitstate: Option<Oid>,
    /// Gitstate tree currently reflected in the local `.git`.
    #[serde(with = "spoko_core::serde_oid::opt", default)]
    pub applied_gitstate: Option<Oid>,
    /// Entries that could not be written here (case collision, Windows name, dir in the way);
    /// kept in every local snapshot so they are not propagated as deletions.
    pub unmaterialized: BTreeMap<RelPath, Entry>,
    /// Conflict paths the user resolved since the last capture.
    pub resolved: BTreeSet<String>,
    pub last_activity_push: i64,
    /// Version of the activity pointer a device-switch prompt was shown for.
    pub switch_prompt_version: u64,
}

impl ProjectState {
    pub fn load(path: &Path) -> std::io::Result<ProjectState> {
        match std::fs::read(path) {
            Ok(b) => serde_json::from_slice(&b).map_err(std::io::Error::other),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ProjectState::default()),
            Err(e) => Err(e),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        spoko_core::platform::atomic_write_barrier(path, &bytes)?;
        Ok(())
    }
}
