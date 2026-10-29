use std::collections::{BTreeMap, BTreeSet};

use git2::Oid;

use super::{GitState, StashEntry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitMergeNote {
    /// Both machines moved the same branch; the older side's tip lives on as `split_as`.
    BranchSplit { name: String, kept: Oid, split_as: String },
}

#[derive(Debug, Clone)]
pub struct GitMergeResult {
    pub state: GitState,
    pub notes: Vec<GitMergeNote>,
}

/// Lowercase `[a-z0-9-]` slug of a device name (for split branch names).
pub fn device_slug(name: &str) -> String {
    let raw: String = name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let parts: Vec<&str> = raw.split('-').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() { "device".to_owned() } else { parts.join("-") }
}

/// Three-way merge of two git states. `ours_newer` decides scalar fields (HEAD, index, files)
/// and which side keeps a branch both moved; `older_device` (the device of the older side)
/// names the branch that preserves the older tip.
pub fn merge_gitstate(base: Option<&GitState>, ours: &GitState, theirs: &GitState, ours_newer: bool, older_device: &str) -> GitMergeResult {
    let (newer, older) = if ours_newer { (ours, theirs) } else { (theirs, ours) };
    let empty = BTreeMap::new();
    let base_refs = base.map_or(&empty, |b| &b.refs);
    let mut refs = BTreeMap::new();
    let mut notes = Vec::new();
    let names: BTreeSet<&String> = base_refs.keys().chain(ours.refs.keys()).chain(theirs.refs.keys()).collect();
    for name in names {
        let (b, o, t) = (base_refs.get(name), ours.refs.get(name), theirs.refs.get(name));
        let pick = match (o, t) {
            _ if o == t => o,
            _ if o == b => t.or(o.filter(|_| b.is_none())),
            _ if t == b => o.or(t.filter(|_| b.is_none())),
            (Some(_), None) => o,
            (None, Some(_)) => t,
            (Some(_), Some(_)) => {
                let (keep, other) = (newer.refs[name], older.refs[name]);
                if name.starts_with("refs/heads/") {
                    let split_as = format!("{name}-{}", device_slug(older_device));
                    notes.push(GitMergeNote::BranchSplit { name: name.clone(), kept: keep, split_as: split_as.clone() });
                    refs.insert(split_as, other);
                }
                Some(&newer.refs[name])
            }
            (None, None) => None,
        };
        if let Some(oid) = pick {
            refs.insert(name.clone(), *oid);
        }
    }

    let base_stash: BTreeSet<Oid> = base.map(|b| b.stash.iter().map(|s| s.oid).collect()).unwrap_or_default();
    let dropped: BTreeSet<Oid> = base_stash
        .iter()
        .filter(|o| !ours.stash.iter().any(|s| s.oid == **o) || !theirs.stash.iter().any(|s| s.oid == **o))
        .copied()
        .collect();
    let mut stash: Vec<StashEntry> = Vec::new();
    for s in newer.stash.iter().chain(older.stash.iter()) {
        if !dropped.contains(&s.oid) && !stash.iter().any(|x| x.oid == s.oid) {
            stash.push(s.clone());
        }
    }

    let mut files = BTreeMap::new();
    let base_files = base.map(|b| &b.files);
    let file_names: BTreeSet<&String> = base_files.into_iter().flat_map(|f| f.keys()).chain(ours.files.keys()).chain(theirs.files.keys()).collect();
    for name in file_names {
        if let Some(e) = pick3(base_files.map(|f| f.get(name)), &ours.files.get(name), &theirs.files.get(name), ours_newer) {
            files.insert(name.clone(), *e);
        }
    }

    GitMergeResult {
        state: GitState {
            head: pick3(base.map(|b| b.head.clone()), &ours.head, &theirs.head, ours_newer),
            refs,
            index_tree: pick3(base.map(|b| b.index_tree), &ours.index_tree, &theirs.index_tree, ours_newer),
            stash,
            files,
        },
        notes,
    }
}

/// Three-way pick of a scalar: the side that changed wins; both changed → the newer side.
fn pick3<T: PartialEq + Clone>(base: Option<T>, ours: &T, theirs: &T, ours_newer: bool) -> T {
    let (o, t) = (ours.clone(), theirs.clone());
    match base {
        _ if o == t => o,
        Some(b) if b == o => t,
        Some(b) if b == t => o,
        _ if ours_newer => o,
        _ => t,
    }
}
