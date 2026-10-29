//! Mirroring the user's git repository state (never writing anything of spoko's into it).

mod apply;
mod capture;
pub mod config_merge;

use std::collections::BTreeMap;
use std::path::PathBuf;

use git2::Oid;
use serde::{Deserialize, Serialize};

pub use apply::{ApplyGitOutcome, apply};
pub use capture::{CaptureStats, capture, capture_with_stats, copy_objects};

use crate::platform::RelPath;
use crate::store::{Entry, EntryMode, Store, WorkMap};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Head {
    /// Full ref name, e.g. `refs/heads/dzban`.
    Branch(String),
    #[serde(with = "crate::serde_oid")]
    Detached(Oid),
    /// Branch with no commits yet.
    Unborn(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StashEntry {
    #[serde(with = "crate::serde_oid")]
    pub oid: Oid,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitState {
    pub head: Head,
    /// Every direct ref under `refs/` except `refs/stash`.
    pub refs: BTreeMap<String, Oid>,
    /// Tree of the staged index (stored in the internal store).
    pub index_tree: Option<Oid>,
    /// Newest first.
    pub stash: Vec<StashEntry>,
    /// Files inside `.git`: `config`, `hooks/*`, `info/exclude`, `lfs/objects/**`.
    pub files: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusyReason {
    Lock(PathBuf),
    InProgress(&'static str),
}

#[derive(Debug)]
pub enum CaptureOutcome {
    Captured(GitState),
    NoRepo,
    Busy(BusyReason),
    Unsupported(&'static str),
}

#[derive(Serialize, Deserialize)]
struct StateJson {
    head: Head,
    refs: BTreeMap<String, String>,
    index_tree: Option<String>,
    stash: Vec<StashEntry>,
}

const STATE_FILE: &str = "state.json";

/// Tree layout: `state.json`, `files/...`, `index` (subtree), `tips/<n>` gitlinks to every
/// referenced commit so that transfer can reach the user's history.
pub fn gitstate_tree(store: &Store, gs: &GitState) -> crate::Result<Oid> {
    let json = StateJson {
        head: gs.head.clone(),
        refs: gs.refs.iter().map(|(k, v)| (k.clone(), v.to_string())).collect(),
        index_tree: gs.index_tree.map(|o| o.to_string()),
        stash: gs.stash.clone(),
    };
    let mut w = WorkMap::default();
    w.insert(RelPath::new(STATE_FILE), Entry { oid: store.write_blob(&serde_json::to_vec_pretty(&json)?)?, mode: EntryMode::Blob });
    for (name, e) in &gs.files {
        w.insert(RelPath::new(&format!("files/{name}")), *e);
    }
    if let Some(t) = gs.index_tree {
        w.insert(RelPath::new("index"), Entry { oid: t, mode: EntryMode::Tree });
    }
    let mut tips: Vec<Oid> = gs.refs.values().copied().chain(gs.stash.iter().map(|s| s.oid)).collect();
    if let Head::Detached(o) = gs.head {
        tips.push(o);
    }
    tips.sort();
    tips.dedup();
    for (i, t) in tips.iter().enumerate() {
        if store.repo().find_commit(*t).is_ok() || store.repo().find_tag(*t).is_ok() {
            w.insert(RelPath::new(&format!("tips/{i}")), Entry { oid: *t, mode: EntryMode::Commit });
        }
    }
    Ok(store.build_tree(&w)?)
}

pub fn read_gitstate(store: &Store, tree: Oid) -> crate::Result<Option<GitState>> {
    let t = store.repo().find_tree(tree)?;
    let Some(state) = t.get_name(STATE_FILE) else { return Ok(None) };
    let json: StateJson = serde_json::from_slice(&store.read_blob(state.id())?)?;
    let parse = |s: &str| Oid::from_str(s).map_err(crate::Error::from);
    let mut files = BTreeMap::new();
    if let Some(f) = store.subtree(tree, "files")? {
        for (p, e) in store.read_tree(f)?.iter() {
            files.insert(p.as_str().to_owned(), *e);
        }
    }
    Ok(Some(GitState {
        head: json.head,
        refs: json.refs.iter().map(|(k, v)| Ok((k.clone(), parse(v)?))).collect::<crate::Result<_>>()?,
        index_tree: json.index_tree.as_deref().map(parse).transpose()?,
        stash: json.stash,
        files,
    }))
}

/// Lock files or an operation in progress mean git is busy: do not read or write `.git` now.
pub fn busy(git_dir: &std::path::Path) -> std::io::Result<Option<BusyReason>> {
    const MARKERS: &[&str] = &["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD", "rebase-merge", "rebase-apply", "BISECT_LOG"];
    for m in MARKERS {
        if git_dir.join(m).exists() {
            return Ok(Some(BusyReason::InProgress(m)));
        }
    }
    let mut stack = vec![git_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir)?.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "lock") {
                return Ok(Some(BusyReason::Lock(p)));
            }
            if e.file_type()?.is_dir() && (dir.as_path() != git_dir || e.file_name() == "refs") {
                stack.push(p);
            }
        }
    }
    Ok(None)
}
