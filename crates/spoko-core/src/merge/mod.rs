//! Three-way merge of snapshot sections with spoko's conflict policy:
//! text → git's file merge; binary → newer side wins; Claude `.jsonl` → union;
//! unresolvable → keep *ours* (the machine you are working on) and record a [`Conflict`].

pub mod jsonl;

use std::collections::{BTreeMap, BTreeSet};

use git2::{MergeFileInput, Oid};
use serde::{Deserialize, Serialize};

use crate::platform::RelPath;
use crate::store::{Entry, Store, WorkMap};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    /// Path inside the snapshot tree, e.g. `work/src/a.rs`.
    pub path: String,
    #[serde(with = "crate::serde_oid::opt")]
    pub base: Option<Oid>,
    #[serde(with = "crate::serde_oid::opt")]
    pub theirs: Option<Oid>,
    pub theirs_device: String,
    pub at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Ours,
    Theirs,
}

pub struct MergeCtx<'a> {
    /// Prefix of this section inside the snapshot (`work/`, `external/claude/`).
    pub prefix: &'a str,
    pub ours_newer: bool,
    pub is_binary: &'a dyn Fn(&RelPath, &[u8], u64) -> bool,
    pub union_jsonl: bool,
    pub theirs_device: String,
    pub now: i64,
}

#[derive(Debug)]
pub struct TreeMerge {
    pub tree: Oid,
    pub new_conflicts: Vec<Conflict>,
    /// Binary paths where one side's version was replaced (side = the version that lost).
    pub binary_overwrites: Vec<(String, Side)>,
}

/// Content and size of a blob; content of huge blobs is not loaded (they are binary by size).
fn head(store: &Store, oid: Oid) -> crate::Result<(Vec<u8>, u64)> {
    const MAX_LOAD: u64 = 64 << 20;
    let size = store.blob_size(oid)?;
    let data = if size <= MAX_LOAD { store.read_blob(oid)? } else { Vec::new() };
    Ok((data, size))
}

pub fn merge_trees(store: &Store, base: Oid, ours: Oid, theirs: Oid, ctx: &MergeCtx<'_>) -> crate::Result<TreeMerge> {
    let (b, o, t) = (store.read_tree(base)?, store.read_tree(ours)?, store.read_tree(theirs)?);
    let mut out = WorkMap::default();
    let mut new_conflicts = Vec::new();
    let mut binary_overwrites = Vec::new();
    let paths: BTreeSet<&RelPath> = b.keys().chain(o.keys()).chain(t.keys()).collect();
    for p in paths {
        let (eb, eo, et) = (b.get(p), o.get(p), t.get(p));
        let full = format!("{}{}", ctx.prefix, p);
        let pick: Option<Entry> = if eo == et || et == eb {
            eo.copied()
        } else if eo == eb {
            et.copied()
        } else {
            match (eo, et) {
                (Some(x), Some(y)) if x.oid == y.oid => Some(if ctx.ours_newer { *x } else { *y }),
                (Some(x), Some(y)) => {
                    let (ox, sx) = head(store, x.oid)?;
                    let (oy, sy) = head(store, y.oid)?;
                    if (ctx.is_binary)(p, &ox, sx) || (ctx.is_binary)(p, &oy, sy) {
                        binary_overwrites.push((full.clone(), if ctx.ours_newer { Side::Theirs } else { Side::Ours }));
                        Some(if ctx.ours_newer { *x } else { *y })
                    } else if ctx.union_jsonl && p.as_str().ends_with(".jsonl") {
                        Some(Entry { oid: store.write_blob(&jsonl::union(&ox, &oy))?, mode: x.mode })
                    } else {
                        let base_bytes = match eb {
                            Some(e) => store.read_blob(e.oid)?,
                            None => Vec::new(),
                        };
                        let mut ib = MergeFileInput::new();
                        ib.content(&base_bytes);
                        let mut io = MergeFileInput::new();
                        io.content(&ox);
                        let mut it = MergeFileInput::new();
                        it.content(&oy);
                        let r = git2::merge_file(&ib, &io, &it, None)?;
                        if r.is_automergeable() {
                            Some(Entry { oid: store.write_blob(r.content())?, mode: x.mode })
                        } else {
                            new_conflicts.push(Conflict { path: full.clone(), base: eb.map(|e| e.oid), theirs: Some(y.oid), theirs_device: ctx.theirs_device.clone(), at: ctx.now });
                            Some(*x)
                        }
                    }
                }
                _ => {
                    new_conflicts.push(Conflict { path: full.clone(), base: eb.map(|e| e.oid), theirs: et.map(|e| e.oid), theirs_device: ctx.theirs_device.clone(), at: ctx.now });
                    eo.copied()
                }
            }
        };
        if let Some(e) = pick {
            out.insert(p.clone(), e);
        }
    }
    Ok(TreeMerge { tree: store.build_tree(&out)?, new_conflicts, binary_overwrites })
}

/// Union of two conflict lists by path (ours first), minus resolved paths.
pub fn carry_conflicts(ours: &[Conflict], theirs: &[Conflict], resolved: &BTreeSet<String>) -> Vec<Conflict> {
    let mut by_path: BTreeMap<&str, &Conflict> = BTreeMap::new();
    for c in theirs.iter().chain(ours.iter()).rev() {
        by_path.entry(&c.path).or_insert(c);
    }
    by_path.into_values().filter(|c| !resolved.contains(&c.path)).cloned().collect()
}
