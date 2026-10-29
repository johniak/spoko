//! Writing a tree onto disk — only if the disk still matches what we expect.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::platform::{self, DiskEntry, DiskKind, NameProblem, RelPath, fold_key, read_entry, windows_problem};
#[cfg(windows)]
use crate::platform::SymlinkOutcome;
use crate::scan::StatCache;
use crate::store::{Entry, EntryMode, Store, WorkMap};
use crate::transform::ContentTransform;

/// Test hook to simulate a crash in the middle of an apply.
pub trait ApplyFaults {
    fn before_write(&self, index: usize, rel: &RelPath) -> io::Result<()>;
}

#[derive(Default, Clone, Copy)]
pub struct ApplyOpts<'a> {
    pub case_insensitive: bool,
    pub windows_names: bool,
    pub faults: Option<&'a dyn ApplyFaults>,
    pub transform: Option<&'a dyn ContentTransform>,
}

impl ApplyOpts<'_> {
    /// Options matching the current platform for `root`.
    pub fn for_root(root: &Path) -> io::Result<ApplyOpts<'static>> {
        Ok(ApplyOpts { case_insensitive: platform::fs_is_case_insensitive(root)?, windows_names: cfg!(windows), faults: None, transform: None })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    CaseCollision(RelPath),
    WindowsName(NameProblem),
    /// A directory with untracked content occupies the path; it is never deleted.
    DirInTheWay,
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SkipReason::CaseCollision(other) => write!(f, "koliduje z '{other}' (system plików nie rozróżnia wielkości liter)"),
            SkipReason::WindowsName(p) => p.fmt(f),
            SkipReason::DirInTheWay => f.write_str("w tym miejscu jest katalog z nieśledzonymi plikami"),
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
fn disk_matches(root: &Path, cache: &StatCache, rel: &RelPath, expected: Option<&Entry>, transform: Option<&dyn ContentTransform>) -> io::Result<bool> {
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
        DiskKind::File if transform.is_some_and(|t| t.applies(rel)) => {
            let bytes = fs::read(rel.to_fs(root))?;
            git2::Oid::hash_object(git2::ObjectType::Blob, &transform.expect("checked").disk_to_snapshot(&bytes))
        }
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

/// Blobs up to this size are written by the pool (bigger ones stream from the store).
const POOL_MAX_BLOB: u64 = 1 << 20;
/// Content buffered for the pool before it is written.
const WAVE_BYTES: usize = 64 << 20;

/// A pooled write: project path, absolute path, content, executable.
type WriteJob<'p> = (&'p RelPath, PathBuf, Vec<u8>, bool);

/// Writes `jobs` on a few threads. Work is handed out a
/// directory at a time: the filesystem serializes creates and renames within one directory.
fn write_pool<'p>(jobs: Vec<WriteJob<'p>>) -> io::Result<Vec<(&'p RelPath, DiskEntry)>> {
    if jobs.is_empty() {
        return Ok(vec![]);
    }
    let mut by_dir: BTreeMap<PathBuf, Vec<WriteJob<'p>>> = BTreeMap::new();
    for j in jobs {
        by_dir.entry(j.1.parent().expect("file has a parent").to_path_buf()).or_default().push(j);
    }
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().clamp(2, 8)).min(by_dir.len());
    let queue = std::sync::Mutex::new(by_dir.into_iter());
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| -> io::Result<Vec<(&'p RelPath, DiskEntry)>> {
                    let mut out = Vec::new();
                    loop {
                        let Some((dir, files)) = queue.lock().expect("write queue").next() else { return Ok(out) };
                        fs::create_dir_all(&dir)?;
                        for (rel, abs, bytes, exec) in files {
                            out.push((rel, platform::atomic_write_in_dir(&abs, exec, &mut |w| w.write_all(&bytes))?));
                        }
                    }
                })
            })
            .collect();
        let mut all = Vec::new();
        for h in handles {
            all.extend(h.join().expect("writer thread panicked")?);
        }
        Ok(all)
    })
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
        if !disk_matches(root, cache, p, from.get(*p), opts.transform)? {
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
    // Plain small blobs are written by a thread pool (open/write/fsync/rename dominate a big
    // checkout); links, transformed and big streamed files, and fault-injected runs stay inline.
    let mut written: BTreeMap<&RelPath, DiskEntry> = BTreeMap::new();
    let mut wave: Vec<WriteJob<'_>> = Vec::new();
    let mut wave_bytes = 0;
    for p in changed.iter().filter(|p| to.contains_key(**p)) {
        let entry = to[*p];
        if let Some(reason) = skip.get(*p) {
            report.skipped.push(((*p).clone(), reason.clone()));
            continue;
        }
        if entry.mode == EntryMode::Commit {
            continue;
        }
        let abs = p.to_fs(root);
        if read_entry(&abs)?.is_some_and(|d| d.kind == DiskKind::Dir) {
            report.skipped.push(((*p).clone(), SkipReason::DirInTheWay));
            continue;
        }
        let pooled = opts.faults.is_none()
            && entry.mode != EntryMode::Link
            && !opts.transform.is_some_and(|t| t.applies(p))
            && store.blob_size(entry.oid)? <= POOL_MAX_BLOB;
        if pooled {
            let bytes = store.read_blob(entry.oid)?;
            wave_bytes += bytes.len();
            wave.push((*p, abs, bytes, entry.mode == EntryMode::BlobExec));
            if wave_bytes >= WAVE_BYTES {
                written.extend(write_pool(std::mem::take(&mut wave))?);
                wave_bytes = 0;
            }
            continue;
        }
        if let Some(f) = opts.faults {
            f.before_write(index, p)?;
        }
        index += 1;
        let disk = if entry.mode == EntryMode::Link {
            let target = String::from_utf8_lossy(&store.read_blob(entry.oid)?).into_owned();
            let _outcome = platform::write_symlink(&abs, &target)?;
            let disk = read_entry(&abs)?.expect("just written");
            // symlinks become placeholder files only on Windows without the privilege
            #[cfg(windows)]
            if _outcome == SymlinkOutcome::Placeholder {
                cache.record_placeholder(p, &disk, entry);
                report.written.push((*p).clone());
                continue;
            }
            disk
        } else if let Some(t) = opts.transform.filter(|t| t.applies(p)) {
            let bytes = t.snapshot_to_disk(&store.read_blob(entry.oid)?);
            platform::atomic_write(&abs, &mut bytes.as_slice(), entry.mode == EntryMode::BlobExec)?
        } else {
            platform::atomic_write_with(&abs, entry.mode == EntryMode::BlobExec, &mut |w| {
                store.copy_blob_to(entry.oid, w).map(|_| ()).map_err(io::Error::other)
            })?
        };
        written.insert(p, disk);
    }
    written.extend(write_pool(wave)?);
    for (p, disk) in written {
        cache.record(p, &disk, to[p]);
        report.written.push(p.clone());
    }
    report.written.sort();
    Ok(report)
}
