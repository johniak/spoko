//! Turning the project directory into a [`WorkMap`], hashing only what changed.

mod statcache;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

pub use statcache::{CacheEntry, RACY_WINDOW_NS, StatCache};

use crate::filter::{Class, Matcher};
use crate::platform::{DiskEntry, DiskKind, RelPath, read_entry};
use crate::store::{Entry, EntryMode, Store, WorkMap};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanProblem {
    Unreadable(String),
    NonUtf8Name(String),
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScanStats {
    pub hashed: usize,
    pub dirs_visited: usize,
}

#[derive(Debug, Default)]
pub struct ScanResult {
    pub work: WorkMap,
    pub problems: Vec<(RelPath, ScanProblem)>,
    pub stats: ScanStats,
}

struct Scanner<'a> {
    root: &'a Path,
    matcher: &'a Matcher,
    store: &'a Store,
    cache: &'a mut StatCache,
    prev: &'a WorkMap,
    out: ScanResult,
}

impl Scanner<'_> {
    fn walk_dir(&mut self, dir: Option<&RelPath>) -> crate::Result<()> {
        let abs = dir.map_or_else(|| self.root.to_path_buf(), |d| d.to_fs(self.root));
        let entries = match fs::read_dir(&abs) {
            Ok(e) => e,
            Err(e) if dir.is_some() => {
                self.out.problems.push((dir.expect("checked").clone(), ScanProblem::Unreadable(e.to_string())));
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };
        self.out.stats.dirs_visited += 1;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                let lossy = name.to_string_lossy().into_owned();
                let rel = dir.map_or_else(|| RelPath::new(&lossy), |d| d.join(&lossy));
                self.out.problems.push((rel, ScanProblem::NonUtf8Name(lossy)));
                continue;
            };
            let rel = dir.map_or_else(|| RelPath::new(name), |d| d.join(name));
            self.visit(&rel)?;
        }
        Ok(())
    }

    fn visit(&mut self, rel: &RelPath) -> crate::Result<()> {
        let Some(disk) = read_entry(&rel.to_fs(self.root))? else { return Ok(()) };
        match disk.kind {
            DiskKind::Dir => {
                if self.matcher.should_descend(rel) {
                    self.walk_dir(Some(rel))?;
                }
            }
            _ if self.matcher.classify(rel, false) == Class::Ignored => {}
            _ => match self.hash(rel, &disk) {
                Ok(entry) => {
                    self.out.work.insert(rel.clone(), entry);
                }
                Err(e) => self.out.problems.push((rel.clone(), ScanProblem::Unreadable(e.to_string()))),
            },
        }
        Ok(())
    }

    fn hash(&mut self, rel: &RelPath, disk: &DiskEntry) -> crate::Result<Entry> {
        if let Some(hit) = self.cache.lookup(rel, disk) {
            return Ok(hit);
        }
        self.out.stats.hashed += 1;
        let prev = self.prev.get(rel);
        let entry = match &disk.kind {
            DiskKind::Symlink(target) => Entry { oid: self.store.write_blob(target.as_bytes())?, mode: EntryMode::Link },
            _ => {
                let mut f = fs::File::open(rel.to_fs(self.root))?;
                let oid = self.store.write_blob_stream(&mut f, disk.size)?;
                let placeholder = self.cache.entries.get(rel).is_some_and(|c| c.symlink_placeholder);
                let exec = if cfg!(windows) { prev.is_some_and(|p| p.mode == EntryMode::BlobExec) } else { disk.exec };
                let mode = if placeholder && prev.is_some_and(|p| p.mode == EntryMode::Link && p.oid == oid) {
                    EntryMode::Link
                } else if exec {
                    EntryMode::BlobExec
                } else {
                    EntryMode::Blob
                };
                Entry { oid, mode }
            }
        };
        self.cache.record(rel, disk, entry);
        Ok(entry)
    }
}

/// Walks the whole project.
pub fn full_scan(root: &Path, matcher: &Matcher, store: &Store, cache: &mut StatCache, prev: &WorkMap) -> crate::Result<ScanResult> {
    let mut s = Scanner { root, matcher, store, cache, prev, out: ScanResult::default() };
    s.walk_dir(None)?;
    let out = s.out;
    cache.entries.retain(|p, _| out.work.contains_key(p));
    Ok(out)
}

/// Rescans only `dirty` paths (files or directories) on top of `prev`.
pub fn incremental_scan(
    root: &Path,
    matcher: &Matcher,
    store: &Store,
    cache: &mut StatCache,
    prev: &WorkMap,
    dirty: &BTreeSet<RelPath>,
) -> crate::Result<ScanResult> {
    let mut work = prev.clone();
    for d in dirty {
        work.retain(|p, _| !p.is_within(d));
        cache.entries.retain(|p, _| !p.is_within(d));
    }
    let mut s = Scanner { root, matcher, store, cache, prev, out: ScanResult { work, ..Default::default() } };
    for d in dirty {
        if !d.components().any(|c| c == ".git") && d.parent().is_none_or(|p| s.ancestors_descend(&p)) {
            s.visit(d)?;
        }
    }
    Ok(s.out)
}

impl Scanner<'_> {
    fn ancestors_descend(&self, dir: &RelPath) -> bool {
        let comps: Vec<&str> = dir.components().collect();
        (1..=comps.len()).all(|n| self.matcher.should_descend(&RelPath::new(&comps[..n].join("/"))))
    }
}
