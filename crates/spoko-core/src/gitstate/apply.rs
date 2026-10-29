use std::collections::HashMap;
use std::path::Path;

use git2::{IndexEntry, IndexTime, ObjectType, Oid, Repository, Signature, TreeWalkMode, TreeWalkResult};

use super::{BusyReason, GitState, Head, busy, config_merge, copy_objects};
use crate::platform::{self, RelPath};
use crate::store::{Entry, EntryMode, Store};

#[derive(Debug)]
pub enum ApplyGitOutcome {
    Applied,
    /// git is busy (lock / operation in progress) or the repo layout is unsupported; retry later.
    Deferred(BusyReason),
}

const REFLOG_MSG: &str = "sync";

/// Makes the repository at `root` mirror `to`. `from` is the state previously applied/captured
/// here (used to delete refs and files that disappeared).
pub fn apply(root: &Path, store: &Store, from: Option<&GitState>, to: &GitState) -> crate::Result<ApplyGitOutcome> {
    let git_dir = root.join(".git");
    match std::fs::symlink_metadata(&git_dir) {
        Ok(m) if !m.is_dir() => return Ok(ApplyGitOutcome::Deferred(BusyReason::InProgress("worktree"))),
        Ok(_) => {
            if let Some(reason) = busy(&git_dir)? {
                return Ok(ApplyGitOutcome::Deferred(reason));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Repository::init(root)?;
        }
        Err(e) => return Err(e.into()),
    }
    let repo = Repository::open(root)?;

    let mut roots: Vec<Oid> = to.refs.values().copied().chain(to.stash.iter().map(|s| s.oid)).collect();
    roots.extend(to.index_tree);
    if let Head::Detached(o) = to.head {
        roots.push(o);
    }
    copy_objects(store.repo(), &repo, &roots)?;

    for (name, oid) in &to.refs {
        if repo.find_reference(name).ok().and_then(|r| r.target()) != Some(*oid) {
            repo.reference(name, *oid, true, REFLOG_MSG)?;
        }
    }
    if let Some(from) = from {
        for name in from.refs.keys().filter(|n| !to.refs.contains_key(*n)) {
            if let Ok(mut r) = repo.find_reference(name) {
                r.delete()?;
            }
        }
    }

    apply_head(&repo, &to.head)?;
    if let Some(tree) = to.index_tree {
        apply_index(&repo, root, tree)?;
    }
    apply_stash(&repo, &to.stash)?;
    apply_files(&git_dir, store, from, to)?;
    Ok(ApplyGitOutcome::Applied)
}

fn apply_head(repo: &Repository, head: &Head) -> crate::Result<()> {
    let current = repo.find_reference("HEAD")?;
    match head {
        Head::Branch(name) | Head::Unborn(name) => {
            if current.symbolic_target().ok().flatten() != Some(name.as_str()) {
                repo.reference_symbolic("HEAD", name, true, REFLOG_MSG)?;
            }
        }
        Head::Detached(oid) => {
            if current.symbolic_target().ok().flatten().is_some() || current.target() != Some(*oid) {
                repo.set_head_detached(*oid)?;
            }
        }
    }
    Ok(())
}

fn stat_entry(root: &Path, path: &str, oid: Oid, mode: u32) -> IndexEntry {
    let mut e = IndexEntry {
        ctime: IndexTime::new(0, 0),
        mtime: IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode,
        uid: 0,
        gid: 0,
        file_size: 0,
        id: oid,
        flags: u16::try_from(path.len().min(0xfff)).expect("masked"),
        flags_extended: 0,
        path: path.as_bytes().to_vec(),
    };
    let abs = RelPath::new(path).to_fs(root);
    let Ok(meta) = std::fs::symlink_metadata(&abs) else { return e };
    let matches = if meta.file_type().is_symlink() {
        std::fs::read_link(&abs).ok().and_then(|t| Oid::hash_object(ObjectType::Blob, t.to_string_lossy().as_bytes()).ok()) == Some(oid)
    } else {
        Oid::hash_file(ObjectType::Blob, &abs).ok() == Some(oid)
    };
    if !matches {
        return e;
    }
    e.mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(IndexTime::new(0, 0), |d| {
        IndexTime::new(i32::try_from(d.as_secs()).unwrap_or(i32::MAX), d.subsec_nanos())
    });
    e.ctime = e.mtime;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        e.ctime = IndexTime::new(i32::try_from(meta.ctime()).unwrap_or(i32::MAX), u32::try_from(meta.ctime_nsec()).unwrap_or(0));
        e.dev = u32::try_from(meta.dev()).unwrap_or(0);
        e.ino = u32::try_from(meta.ino() & 0xffff_ffff).expect("masked");
        e.uid = meta.uid();
        e.gid = meta.gid();
    }
    e.file_size = u32::try_from(meta.len()).unwrap_or(u32::MAX);
    e
}

/// Sets the index to `tree`, keeping stat data of unchanged entries and filling it for entries
/// whose working-tree file already matches, so `git status` does not have to re-hash everything.
fn apply_index(repo: &Repository, root: &Path, tree: Oid) -> crate::Result<()> {
    let mut index = repo.index()?;
    if index.write_tree().ok() == Some(tree) {
        return Ok(());
    }
    let old: HashMap<Vec<u8>, IndexEntry> = index.iter().map(|e| (e.path.clone(), e)).collect();
    let tree = repo.find_tree(tree)?;
    let mut entries = Vec::new();
    tree.walk(TreeWalkMode::PreOrder, |dir, te| {
        if te.kind() != Some(ObjectType::Tree)
            && let Ok(name) = te.name()
        {
            let path = format!("{dir}{name}");
            let mode = u32::try_from(te.filemode()).unwrap_or(0o100644);
            let e = match old.get(path.as_bytes()) {
                Some(o) if o.id == te.id() && o.mode == mode => clone_entry(o),
                _ => stat_entry(root, &path, te.id(), mode),
            };
            entries.push(e);
        }
        TreeWalkResult::Ok
    })?;
    index.clear()?;
    for e in &entries {
        index.add(e)?;
    }
    index.write()?;
    Ok(())
}

fn clone_entry(e: &IndexEntry) -> IndexEntry {
    IndexEntry {
        ctime: e.ctime,
        mtime: e.mtime,
        dev: e.dev,
        ino: e.ino,
        mode: e.mode,
        uid: e.uid,
        gid: e.gid,
        file_size: e.file_size,
        id: e.id,
        flags: e.flags,
        flags_extended: e.flags_extended,
        path: e.path.clone(),
    }
}

fn signature(repo: &Repository) -> Result<Signature<'static>, git2::Error> {
    repo.signature().or_else(|_| Signature::now("spoko", "spoko@localhost"))
}

fn apply_stash(repo: &Repository, stash: &[super::StashEntry]) -> crate::Result<()> {
    let current: Vec<Oid> = if repo.find_reference("refs/stash").is_ok() {
        repo.reflog("refs/stash")?.iter().map(|e| e.id_new()).collect()
    } else {
        vec![]
    };
    let wanted: Vec<Oid> = stash.iter().map(|s| s.oid).collect();
    if current == wanted {
        return Ok(());
    }
    if wanted.is_empty() {
        repo.find_reference("refs/stash")?.delete()?;
        let _ = repo.reflog_delete("refs/stash");
        return Ok(());
    }
    repo.reference("refs/stash", wanted[0], true, REFLOG_MSG)?;
    let mut reflog = repo.reflog("refs/stash")?;
    while !reflog.is_empty() {
        reflog.remove(0, false)?;
    }
    let sig = signature(repo)?;
    for s in stash.iter().rev() {
        reflog.append(s.oid, &sig, Some(&s.message))?;
    }
    reflog.write()?;
    Ok(())
}

fn write_git_file(git_dir: &Path, store: &Store, name: &str, e: &Entry) -> crate::Result<()> {
    let abs = RelPath::new(name).to_fs(git_dir);
    let same = Oid::hash_file(ObjectType::Blob, &abs).ok() == Some(e.oid);
    let exec_ok = platform::read_entry(&abs)?.is_some_and(|d| !cfg!(unix) || d.exec == (e.mode == EntryMode::BlobExec));
    if same && exec_ok {
        return Ok(());
    }
    platform::atomic_write_with(&abs, e.mode == EntryMode::BlobExec, &mut |w| {
        store.copy_blob_to(e.oid, w).map(|_| ()).map_err(std::io::Error::other)
    })?;
    Ok(())
}

fn apply_files(git_dir: &Path, store: &Store, from: Option<&GitState>, to: &GitState) -> crate::Result<()> {
    for (name, e) in &to.files {
        if name == "config" {
            let merged = config_merge::merge(&store.read_blob(e.oid)?, &git_dir.join("config"))?;
            if std::fs::read(git_dir.join("config")).ok().as_deref() != Some(merged.as_slice()) {
                platform::atomic_write(&git_dir.join("config"), &mut merged.as_slice(), false)?;
            }
        } else {
            write_git_file(git_dir, store, name, e)?;
        }
    }
    if let Some(from) = from {
        for name in from.files.keys().filter(|n| !to.files.contains_key(*n) && *n != "config") {
            platform::remove_file(&RelPath::new(name).to_fs(git_dir))?;
        }
    }
    Ok(())
}
