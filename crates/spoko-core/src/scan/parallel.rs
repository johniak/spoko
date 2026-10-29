//! Parallel directory walk for full scans: `lstat`, filtering, stat-cache lookups and reading
//! small changed files run on several threads; hashing into the store stays on the caller's.

use std::fs;
use std::path::Path;
use std::sync::{Condvar, Mutex};

use super::{ScanProblem, StatCache};
use crate::filter::{Class, Matcher};
use crate::platform::{DiskEntry, DiskKind, RelPath, read_entry};
use crate::store::Entry;

/// Files at most this big are read on the walker threads.
const PRELOAD_MAX: u64 = 1 << 20;

pub(super) struct Found {
    pub rel: RelPath,
    pub disk: DiskEntry,
    /// Stat-cache hit: no need to read the file.
    pub hit: Option<Entry>,
    /// Content of a small regular file that missed the cache.
    pub bytes: Option<Vec<u8>>,
}

#[derive(Default)]
pub(super) struct Walked {
    pub found: Vec<Found>,
    pub problems: Vec<(RelPath, ScanProblem)>,
    pub dirs_visited: usize,
}

struct Shared<'a> {
    root: &'a Path,
    matcher: &'a Matcher,
    cache: &'a StatCache,
    /// Directories to read (`None` = root) and the number of workers busy.
    queue: Mutex<(Vec<Option<RelPath>>, usize)>,
    cv: Condvar,
    error: Mutex<Option<crate::Error>>,
}

impl Shared<'_> {
    fn next(&self) -> Option<Option<RelPath>> {
        let mut q = self.queue.lock().expect("scan queue");
        loop {
            if self.error.lock().expect("scan error").is_some() {
                return None;
            }
            if let Some(d) = q.0.pop() {
                q.1 += 1;
                return Some(d);
            }
            if q.1 == 0 {
                return None;
            }
            q = self.cv.wait(q).expect("scan queue");
        }
    }

    fn done(&self, subdirs: Vec<Option<RelPath>>) {
        let mut q = self.queue.lock().expect("scan queue");
        q.0.extend(subdirs);
        q.1 -= 1;
        self.cv.notify_all();
    }

    fn fail(&self, e: crate::Error) {
        self.error.lock().expect("scan error").get_or_insert(e);
        self.cv.notify_all();
    }

    fn read_dir(&self, dir: Option<&RelPath>, out: &mut Walked, subdirs: &mut Vec<Option<RelPath>>) -> crate::Result<()> {
        let abs = dir.map_or_else(|| self.root.to_path_buf(), |d| d.to_fs(self.root));
        let entries = match fs::read_dir(&abs) {
            Ok(e) => e,
            Err(e) if dir.is_some() => {
                out.problems.push((dir.expect("checked").clone(), ScanProblem::Unreadable(e.to_string())));
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };
        out.dirs_visited += 1;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                let lossy = name.to_string_lossy().into_owned();
                let rel = dir.map_or_else(|| RelPath::new(&lossy), |d| d.join(&lossy));
                out.problems.push((rel, ScanProblem::NonUtf8Name(lossy)));
                continue;
            };
            let rel = dir.map_or_else(|| RelPath::new(name), |d| d.join(name));
            let abs = rel.to_fs(self.root);
            let Some(disk) = read_entry(&abs)? else { continue };
            if disk.kind == DiskKind::Dir {
                if self.matcher.should_descend(&rel) {
                    subdirs.push(Some(rel));
                }
                continue;
            }
            if self.matcher.classify(&rel, false) == Class::Ignored {
                continue;
            }
            let hit = self.cache.lookup(&rel, &disk);
            let bytes = (hit.is_none() && disk.kind == DiskKind::File && disk.size <= PRELOAD_MAX)
                .then(|| fs::read(&abs).ok().filter(|b| b.len() as u64 == disk.size))
                .flatten();
            out.found.push(Found { rel, disk, hit, bytes });
        }
        Ok(())
    }
}

pub(super) fn walk(root: &Path, matcher: &Matcher, cache: &StatCache) -> crate::Result<Walked> {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().clamp(2, 8));
    let shared = Shared { root, matcher, cache, queue: Mutex::new((vec![None], 0)), cv: Condvar::new(), error: Mutex::new(None) };
    let parts: Vec<Walked> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut out = Walked::default();
                    while let Some(dir) = shared.next() {
                        let mut subdirs = Vec::new();
                        if let Err(e) = shared.read_dir(dir.as_ref(), &mut out, &mut subdirs) {
                            shared.fail(e);
                        }
                        shared.done(subdirs);
                    }
                    out
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("scan worker panicked")).collect()
    });
    if let Some(e) = shared.error.into_inner().expect("scan error") {
        return Err(e);
    }
    let mut all = Walked::default();
    for p in parts {
        all.found.extend(p.found);
        all.problems.extend(p.problems);
        all.dirs_visited += p.dirs_visited;
    }
    all.problems.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(all)
}
