use std::collections::BTreeMap;
use std::path::Path;

use git2::{ObjectType, Oid, Repository};

use super::{BusyReason, CaptureOutcome, GitState, Head, StashEntry, busy, config_merge};
use crate::platform::RelPath;
use crate::store::{Entry, EntryMode, Store};

#[derive(Debug, Default, Clone, Copy)]
pub struct CaptureStats {
    pub objects_copied: usize,
}

pub fn capture(root: &Path, store: &Store) -> crate::Result<CaptureOutcome> {
    Ok(capture_with_stats(root, store)?.0)
}

pub fn capture_with_stats(root: &Path, store: &Store) -> crate::Result<(CaptureOutcome, CaptureStats)> {
    let git_dir = root.join(".git");
    let mut stats = CaptureStats::default();
    let meta = match std::fs::symlink_metadata(&git_dir) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((CaptureOutcome::NoRepo, stats)),
        Err(e) => return Err(e.into()),
    };
    if !meta.is_dir() {
        return Ok((CaptureOutcome::Unsupported("worktree"), stats));
    }
    if let Some(reason) = busy(&git_dir)? {
        return Ok((CaptureOutcome::Busy(reason), stats));
    }
    let repo = Repository::open(root)?;
    if repo.config()?.get_string("extensions.objectformat").is_ok_and(|f| f.eq_ignore_ascii_case("sha256")) {
        return Ok((CaptureOutcome::Unsupported("sha256"), stats));
    }

    let head_ref = repo.find_reference("HEAD")?;
    let head = match head_ref.symbolic_target() {
        Ok(Some(name)) if repo.find_reference(name).is_ok() => Head::Branch(name.to_owned()),
        Ok(Some(name)) => Head::Unborn(name.to_owned()),
        _ => Head::Detached(head_ref.target().expect("direct HEAD has a target")),
    };

    let mut refs = BTreeMap::new();
    for r in repo.references()? {
        let r = r?;
        if let (Ok(name), Some(target)) = (r.name(), r.target())
            && name != "refs/stash"
        {
            refs.insert(name.to_owned(), target);
        }
    }

    let mut stash = Vec::new();
    if repo.find_reference("refs/stash").is_ok() {
        for e in repo.reflog("refs/stash")?.iter() {
            stash.push(StashEntry { oid: e.id_new(), message: e.message().ok().flatten().unwrap_or("").to_owned() });
        }
    }

    let mut roots: Vec<Oid> = refs.values().copied().chain(stash.iter().map(|s| s.oid)).collect();
    if let Head::Detached(o) = head {
        roots.push(o);
    }
    stats.objects_copied += copy_closure(&repo, store, &roots)?;

    let mut index = repo.index()?;
    for e in index.iter() {
        stats.objects_copied += copy_closure(&repo, store, &[e.id])?;
    }
    let index_tree = match index.write_tree_to(store.repo()) {
        Ok(t) => Some(t),
        Err(_) if index.has_conflicts() => return Ok((CaptureOutcome::Busy(BusyReason::InProgress("index conflicts")), stats)),
        Err(e) => return Err(e.into()),
    };

    let files = capture_files(&git_dir, store)?;
    Ok((CaptureOutcome::Captured(GitState { head, refs, index_tree, stash, files }), stats))
}

/// Copies every object reachable from `roots` into the store. Objects are written only after
/// their dependencies, so `store.has(x)` implies the store holds x's whole closure.
pub fn copy_closure(repo: &Repository, store: &Store, roots: &[Oid]) -> crate::Result<usize> {
    let src = repo.odb()?;
    let mut copied = 0;
    let mut stack: Vec<(Oid, bool)> = roots.iter().map(|o| (*o, false)).collect();
    while let Some((oid, expanded)) = stack.pop() {
        if store.has(oid) {
            continue;
        }
        let Ok(obj) = src.read(oid) else { continue }; // shallow / partial clone: object absent
        if expanded {
            store.write_raw(obj.kind(), obj.data())?;
            copied += 1;
            continue;
        }
        stack.push((oid, true));
        match obj.kind() {
            ObjectType::Commit => {
                let c = repo.find_commit(oid)?;
                stack.push((c.tree_id(), false));
                stack.extend(c.parent_ids().map(|p| (p, false)));
            }
            ObjectType::Tree => {
                let t = repo.find_tree(oid)?;
                for e in t.iter() {
                    if e.kind() != Some(ObjectType::Commit) {
                        stack.push((e.id(), false));
                    }
                }
            }
            ObjectType::Tag => stack.push((repo.find_tag(oid)?.target_id(), false)),
            _ => {}
        }
    }
    Ok(copied)
}

fn capture_files(git_dir: &Path, store: &Store) -> crate::Result<BTreeMap<String, Entry>> {
    let mut files = BTreeMap::new();
    if let Ok(cfg) = std::fs::read(git_dir.join("config")) {
        let stripped = config_merge::strip_local(&cfg)?;
        files.insert("config".to_owned(), Entry { oid: store.write_blob(&stripped)?, mode: EntryMode::Blob });
    }
    if let Ok(exclude) = std::fs::read(git_dir.join("info/exclude")) {
        files.insert("info/exclude".to_owned(), Entry { oid: store.write_blob(&exclude)?, mode: EntryMode::Blob });
    }
    for (sub, skip_samples) in [("hooks", true), ("lfs/objects", false)] {
        let base = git_dir.join(sub);
        let mut stack = vec![base.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if e.file_type()?.is_dir() {
                    stack.push(p);
                    continue;
                }
                let Some(rel) = RelPath::from_fs(git_dir, &p) else { continue };
                if skip_samples && rel.as_str().ends_with(".sample") {
                    continue;
                }
                let disk = crate::platform::read_entry(&p)?.expect("listed");
                let mut f = std::fs::File::open(&p)?;
                let oid = store.write_blob_stream(&mut f, disk.size)?;
                let mode = if disk.exec { EntryMode::BlobExec } else { EntryMode::Blob };
                files.insert(rel.as_str().to_owned(), Entry { oid, mode });
            }
        }
    }
    Ok(files)
}
