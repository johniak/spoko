//! Writing a tree onto disk — only if the disk still matches what we expect.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::Path;

use crate::platform::{self, DiskKind, NameProblem, RelPath, SymlinkOutcome, fold_key, read_entry, windows_problem};
use crate::scan::StatCache;
use crate::store::{Entry, EntryMode, Store, WorkMap};

/// Test hook to simulate a crash in the middle of an apply.
pub trait ApplyFaults {
    fn before_write(&self, index: usize, rel: &RelPath) -> io::Result<()>;
}

#[derive(Default, Clone, Copy)]
pub struct ApplyOpts<'a> {
    pub case_insensitive: bool,
    pub windows_names: bool,
    pub faults: Option<&'a dyn ApplyFaults>,
}

impl ApplyOpts<'_> {
    /// Options matching the current platform for `root`.
    pub fn for_root(root: &Path) -> io::Result<ApplyOpts<'static>> {
        Ok(ApplyOpts { case_insensitive: platform::fs_is_case_insensitive(root)?, windows_names: cfg!(windows), faults: None })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    CaseCollision(RelPath),
    WindowsName(NameProblem),
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SkipReason::CaseCollision(other) => write!(f, "koliduje z '{other}' (system plików nie rozróżnia wielkości liter)"),
            SkipReason::WindowsName(p) => p.fmt(f),
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ApplyReport {
    pub written: Vec<RelPath>,
    pub deleted: Vec<RelPath>,
    pub skipped: Vec<(RelPath, SkipReason)>,
}

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    /// The disk no longer matches `from` at these paths; nothing was written.
    #[error("local changes at {0:?}")]
    LocalChanged(Vec<RelPath>),
    #[error(transparent)]
    Core(#[from] crate::Error),
}

impl From<io::Error> for ApplyError {
    fn from(e: io::Error) -> Self {
        ApplyError::Core(e.into())
    }
}

impl From<git2::Error> for ApplyError {
    fn from(e: git2::Error) -> Self {
        ApplyError::Core(e.into())
    }
}

/// Does the disk at `rel` hold exactly `expected` (or nothing, for `None`)?
fn disk_matches(root: &Path, cache: &StatCache, rel: &RelPath, expected: Option<&Entry>) -> io::Result<bool> {
    let disk = read_entry(&rel.to_fs(root))?;
    let (disk, expected) = match (disk, expected) {
        (None, None) => return Ok(true),
        (Some(d), None) => return Ok(d.kind == DiskKind::Dir),
        (None, Some(e)) => return Ok(e.mode == EntryMode::Commit),
        (Some(d), Some(e)) => (d, e),
    };
    if expected.mode == EntryMode::Commit {
        return Ok(true);
    }
    if cache.lookup(rel, &disk).is_some_and(|c| c.oid == expected.oid) {
        return Ok(true);
    }
    let oid = match &disk.kind {
        DiskKind::Dir => return Ok(false),
        DiskKind::Symlink(t) => git2::Oid::hash_object(git2::ObjectType::Blob, t.as_bytes()),
        DiskKind::File => git2::Oid::hash_file(git2::ObjectType::Blob, rel.to_fs(root)),
    };
    Ok(oid.is_ok_and(|o| o == expected.oid))
}

fn remove_empty_parents(root: &Path, rel: &RelPath) {
    let mut cur = rel.parent();
    while let Some(dir) = cur {
        if fs::remove_dir(dir.to_fs(root)).is_err() {
            break;
        }
        cur = dir.parent();
    }
}

pub fn apply_tree(
    root: &Path,
    store: &Store,
    cache: &mut StatCache,
    from: &WorkMap,
    to: &WorkMap,
    opts: &ApplyOpts<'_>,
) -> Result<ApplyReport, ApplyError> {
    let changed: BTreeSet<&RelPath> = from.keys().chain(to.keys()).filter(|p| from.get(*p) != to.get(*p)).collect();

    let mut conflicts = Vec::new();
    for p in &changed {
        if !disk_matches(root, cache, p, from.get(*p))? {
            conflicts.push((*p).clone());
        }
    }
    if !conflicts.is_empty() {
        return Err(ApplyError::LocalChanged(conflicts));
    }

    let mut report = ApplyReport::default();
    let mut skip: BTreeMap<&RelPath, SkipReason> = BTreeMap::new();
    if opts.windows_names {
        for p in to.keys() {
            if let Some(problem) = windows_problem(p) {
                skip.insert(p, SkipReason::WindowsName(problem));
            }
        }
    }
    if opts.case_insensitive {
        let mut first: BTreeMap<String, &RelPath> = BTreeMap::new();
        let candidates: Vec<&RelPath> = to.keys().filter(|p| !skip.contains_key(p)).collect();
        for p in candidates {
            match first.get(&fold_key(p)) {
                Some(owner) => {
                    skip.insert(p, SkipReason::CaseCollision((*owner).clone()));
                }
                None => {
                    first.insert(fold_key(p), p);
                }
            }
        }
    }

    for p in changed.iter().filter(|p| !to.contains_key(**p)) {
        if from.get(*p).is_some_and(|e| e.mode != EntryMode::Commit) {
            platform::remove_file(&p.to_fs(root))?;
            remove_empty_parents(root, p);
            report.deleted.push((*p).clone());
        }
        cache.entries.remove(*p);
    }

    let mut index = 0;
    for p in changed.iter().filter(|p| to.contains_key(**p)) {
        let entry = to[*p];
        if let Some(reason) = skip.get(*p) {
            report.skipped.push(((*p).clone(), reason.clone()));
            continue;
        }
        if entry.mode == EntryMode::Commit {
            continue;
        }
        if let Some(f) = opts.faults {
            f.before_write(index, p)?;
        }
        index += 1;
        let abs = p.to_fs(root);
        if let Some(d) = read_entry(&abs)?
            && d.kind == DiskKind::Dir
        {
            fs::remove_dir_all(&abs)?;
        }
        let disk = if entry.mode == EntryMode::Link {
            let target = String::from_utf8_lossy(&store.read_blob(entry.oid)?).into_owned();
            let outcome = platform::write_symlink(&abs, &target)?;
            let disk = read_entry(&abs)?.expect("just written");
            if outcome == SymlinkOutcome::Placeholder {
                cache.record_placeholder(p, &disk, entry);
                report.written.push((*p).clone());
                continue;
            }
            disk
        } else {
            platform::atomic_write_with(&abs, entry.mode == EntryMode::BlobExec, &mut |w| {
                store.copy_blob_to(entry.oid, w).map(|_| ()).map_err(io::Error::other)
            })?
        };
        cache.record(p, &disk, entry);
        report.written.push((*p).clone());
    }
    Ok(report)
}
