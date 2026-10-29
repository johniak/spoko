//! File watching with debouncing: a burst of events becomes one batch per project root.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, unbounded};
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};
use spoko_core::platform::{RelPath, is_tmp_name};

use crate::{ClientError, Result};

/// Changes under one watched root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FsBatch {
    pub root: PathBuf,
    pub paths: BTreeSet<RelPath>,
    /// Something under `.git/` changed.
    pub git_dirty: bool,
    /// Events were lost (overflow / watcher error): rescan everything.
    pub rescan: bool,
}

pub const QUIET: Duration = Duration::from_millis(150);
pub const MAX_DELAY: Duration = Duration::from_millis(500);
/// `.git` churns during commits/checkouts (lock files): wait for it to settle.
pub const GIT_QUIET: Duration = Duration::from_millis(500);
pub const GIT_MAX_DELAY: Duration = Duration::from_secs(2);

struct Pending {
    first: Instant,
    last: Instant,
    batch: FsBatch,
}

/// Pure debouncing logic (time is passed in, so it is testable).
pub struct Debouncer {
    /// (watched path as reported by the OS, root to report)
    roots: Vec<(PathBuf, PathBuf)>,
    pending: BTreeMap<PathBuf, Pending>,
}

impl Debouncer {
    pub fn new(roots: &[PathBuf]) -> Debouncer {
        let mut pairs = Vec::new();
        for r in roots {
            pairs.push((r.clone(), r.clone()));
            // macOS reports canonical paths (/private/var/... for /var/...)
            if let Ok(c) = r.canonicalize()
                && &c != r
            {
                pairs.push((c, r.clone()));
            }
        }
        // longest first so nested roots win
        pairs.sort_by_key(|(w, _)| std::cmp::Reverse(w.as_os_str().len()));
        Debouncer { roots: pairs, pending: BTreeMap::new() }
    }

    fn entry(&mut self, root: &Path, now: Instant) -> &mut Pending {
        let p = self.pending.entry(root.to_path_buf()).or_insert_with(|| Pending { first: now, last: now, batch: FsBatch { root: root.to_path_buf(), ..Default::default() } });
        p.last = now;
        p
    }

    pub fn push(&mut self, abs: &Path, now: Instant) {
        let Some((watched, root)) = self.roots.iter().find(|(w, _)| abs.starts_with(w)).cloned() else { return };
        let Some(rel) = RelPath::from_fs(&watched, abs) else {
            if abs == watched {
                self.entry(&root, now).batch.rescan = true;
            }
            return;
        };
        if rel.components().next() == Some(".git") {
            self.entry(&root, now).batch.git_dirty = true;
        } else if !is_tmp_name(rel.file_name()) {
            self.entry(&root, now).batch.paths.insert(rel);
        }
    }

    pub fn push_rescan(&mut self, now: Instant) {
        for root in self.roots.iter().map(|(_, r)| r.clone()).collect::<BTreeSet<_>>() {
            self.entry(&root, now).batch.rescan = true;
        }
    }

    fn limits(p: &Pending) -> (Duration, Duration) {
        if p.batch.paths.is_empty() && !p.batch.rescan { (GIT_QUIET, GIT_MAX_DELAY) } else { (QUIET, MAX_DELAY) }
    }

    pub fn flush_due(&mut self, now: Instant) -> Vec<FsBatch> {
        let due: Vec<PathBuf> = self
            .pending
            .iter()
            .filter(|(_, p)| {
                let (quiet, max) = Self::limits(p);
                now.duration_since(p.last) >= quiet || now.duration_since(p.first) >= max
            })
            .map(|(k, _)| k.clone())
            .collect();
        due.into_iter().filter_map(|k| self.pending.remove(&k)).map(|p| p.batch).collect()
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending
            .values()
            .map(|p| {
                let (quiet, max) = Self::limits(p);
                (p.last + quiet).min(p.first + max)
            })
            .min()
    }
}

pub struct Watcher {
    _inner: RecommendedWatcher,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Watcher {
    /// Watches `roots` recursively (missing ones are skipped; the periodic rescan covers them).
    pub fn new(roots: Vec<PathBuf>) -> Result<(Watcher, Receiver<FsBatch>)> {
        let (raw_tx, raw_rx) = unbounded::<notify::Result<notify::Event>>();
        let mut inner = notify::recommended_watcher(move |ev| {
            let _ = raw_tx.send(ev);
        })
        .map_err(|e| ClientError::Other(format!("watcher: {e}")))?;
        for r in roots.iter().filter(|r| r.is_dir()) {
            inner.watch(r, RecursiveMode::Recursive).map_err(|e| ClientError::Other(format!("watch {}: {e}", r.display())))?;
        }
        let (tx, rx) = unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        let thread = std::thread::spawn(move || debounce_loop(&roots, &raw_rx, &tx, &st));
        Ok((Watcher { _inner: inner, stop, thread: Some(thread) }, rx))
    }
}

fn debounce_loop(roots: &[PathBuf], raw: &Receiver<notify::Result<notify::Event>>, out: &Sender<FsBatch>, stop: &AtomicBool) {
    let mut d = Debouncer::new(roots);
    while !stop.load(Ordering::SeqCst) {
        let wait = d.next_deadline().map_or(Duration::from_millis(200), |t| t.saturating_duration_since(Instant::now())).min(Duration::from_millis(200));
        match raw.recv_timeout(wait) {
            Ok(Ok(ev)) if ev.need_rescan() => d.push_rescan(Instant::now()),
            Ok(Ok(ev)) => {
                let now = Instant::now();
                for p in &ev.paths {
                    d.push(p, now);
                }
            }
            Ok(Err(_)) => d.push_rescan(Instant::now()),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
        for b in d.flush_due(Instant::now()) {
            if out.send(b).is_err() {
                return;
            }
        }
    }
}
