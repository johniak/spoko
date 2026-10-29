//! Per-project sync engine: capture the disk into a snapshot, exchange snapshots with the
//! server, merge when both sides moved, and apply the result back to disk.

mod join;
mod state;
mod sync;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use git2::Oid;
use spoko_core::checkout::{ApplyError, ApplyFaults, ApplyOpts, apply_tree};
use spoko_core::config::{CONFIG_FILE, ProjectConfig};
use spoko_core::external::{ExternalDir, ExternalEnv, PathRewrite, resolve_externals};
use spoko_core::filter::{Class, Matcher};
use spoko_core::gitstate::{
    self, ApplyGitOutcome, CaptureOutcome, GitState, gitstate_tree, read_gitstate,
};
use spoko_core::guard::GuardVerdict;
use spoko_core::merge::carry_conflicts;
use spoko_core::platform::{self, RelPath, read_entry};
use spoko_core::scan::{StatCache, full_scan_with, incremental_scan};
use spoko_core::snapshot::{SnapshotMeta, SnapshotParts, read_snapshot, write_snapshot};
use spoko_core::store::{Store, WorkMap};
use spoko_crypto::Keyring;
use uuid::Uuid;

pub use join::JoinReport;
pub use state::{PauseReason, ProjectState};
pub use sync::SyncOutcome;

use crate::account::Identity;
use crate::api::ApiClient;
use crate::notifier::{Notification, Notifier};
use crate::transfer::known::KnownSet;
use crate::{ClientError, Result};

pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Everything about this device an engine needs (shared by all projects).
#[derive(Clone)]
pub struct DeviceCtx {
    pub home: PathBuf,
    pub device_id: Uuid,
    pub device_name: String,
    pub keyring: Keyring,
    pub api: ApiClient,
    pub env: ExternalEnv,
    pub notifier: Arc<dyn Notifier>,
    pub clock: Clock,
    /// Overrides filesystem case-sensitivity detection (tests).
    pub case_insensitive: Option<bool>,
}

impl DeviceCtx {
    pub fn new(
        home: &Path,
        id: &Identity,
        env: ExternalEnv,
        notifier: Arc<dyn Notifier>,
    ) -> DeviceCtx {
        DeviceCtx {
            home: home.to_path_buf(),
            device_id: id.device_id,
            device_name: id.device_name.clone(),
            keyring: id.keyring.clone(),
            api: id.api(),
            env,
            notifier,
            clock: Arc::new(unix_now),
            case_insensitive: None,
        }
    }
}

/// Present in the project's data dir while an apply runs (see `Engine::apply_snapshot`).
const APPLY_MARKER: &str = "applying";

pub fn project_dir(home: &Path, project: Uuid) -> PathBuf {
    home.join("projects").join(project.to_string())
}

fn is_config_path(p: &RelPath) -> bool {
    p.as_str() == CONFIG_FILE || matches!(p.file_name(), ".gitignore" | ".gitattributes")
}

/// Outcome of applying a snapshot to disk.
enum Applied {
    Done,
    /// The disk changed since the last capture; recapture and retry.
    LocalChanged,
}

pub struct Engine {
    pub project_id: Uuid,
    pub root: PathBuf,
    pub ctx: DeviceCtx,
    pub store: Store,
    pub state: ProjectState,
    /// Test hook: fails writes during apply.
    pub faults: Option<Box<dyn ApplyFaults + Send + Sync>>,
    /// Test hook: runs once right before the next apply from the network.
    pub before_apply: Option<Box<dyn FnOnce() + Send>>,
    /// Test hook: runs right before each write to the user's `.git` and the re-read after it
    /// (another git process may take its lock in between).
    pub before_git: Option<Box<dyn Fn() + Send>>,
    dir: PathBuf,
    cfg: ProjectConfig,
    matcher: Matcher,
    statcache: StatCache,
    /// Work map of `state.last_local`.
    work: WorkMap,
    externals: Vec<ExternalDir>,
    ext_caches: BTreeMap<String, StatCache>,
    ext_work: BTreeMap<String, WorkMap>,
    known: KnownSet,
    case_insensitive: bool,
    /// Watch hooks touched by the apply in progress.
    fresh_hooks: Vec<usize>,
    /// When `statcache.bin` was last written; big caches are written at most every
    /// [`STATCACHE_SAVE_EVERY`] and when the engine is dropped.
    statcache_saved: Option<std::time::Instant>,
}

/// Stat caches with fewer entries are written on every save.
const STATCACHE_ALWAYS_SAVE: usize = 10_000;
const STATCACHE_SAVE_EVERY: std::time::Duration = std::time::Duration::from_secs(30);

impl Drop for Engine {
    fn drop(&mut self) {
        // best effort: a stale cache only costs re-hashing the files changed since
        if self.statcache_saved.is_some() {
            let _ = self.statcache.save(&self.dir.join("statcache.bin"));
        }
    }
}

impl Engine {
    fn load(ctx: DeviceCtx, project_id: Uuid, root: &Path) -> Result<Engine> {
        let dir = project_dir(&ctx.home, project_id);
        std::fs::create_dir_all(&dir)?;
        let store = Store::open_or_init(&dir.join("store.git"))?;
        let state = ProjectState::load(&dir.join("state.json"))?;
        let cfg = ProjectConfig::load(root).map_err(spoko_core::Error::from)?;
        let matcher = Matcher::new(root, &cfg).map_err(spoko_core::Error::from)?;
        let statcache = StatCache::load(&dir.join("statcache.bin"))?;
        let known = KnownSet::load(&dir.join("known.bin"))?;
        let externals = resolve_externals(&cfg, root, &ctx.env);
        let case_insensitive = match ctx.case_insensitive {
            Some(v) => v,
            None if root.is_dir() => platform::fs_is_case_insensitive(root)?,
            None => false,
        };
        let mut e = Engine {
            project_id,
            root: root.to_path_buf(),
            ctx,
            store,
            state,
            faults: None,
            before_apply: None,
            before_git: None,
            dir,
            cfg,
            matcher,
            statcache,
            work: WorkMap::default(),
            externals,
            ext_caches: BTreeMap::new(),
            ext_work: BTreeMap::new(),
            known,
            case_insensitive,
            fresh_hooks: Vec::new(),
            statcache_saved: None,
        };
        for x in &e.externals {
            e.ext_caches.insert(
                x.name.clone(),
                StatCache::load(&e.dir.join(format!("ext-{}.cache", x.name)))?,
            );
        }
        if let Some(last) = e.state.last_local {
            e.load_work_maps(last)?;
        }
        Ok(e)
    }

    fn load_work_maps(&mut self, snapshot: Oid) -> Result<()> {
        let parts = read_snapshot(&self.store, snapshot)?;
        self.work = self.store.read_tree(parts.work)?;
        self.ext_work = parts
            .externals
            .iter()
            .map(|(n, t)| Ok((n.clone(), self.store.read_tree(*t)?)))
            .collect::<Result<_>>()?;
        Ok(())
    }

    /// Opens an existing local project and removes temp files an interrupted apply may have left.
    /// Opens a project another process (the daemon) runs, for reading: no crash cleanup,
    /// which would delete the files of an apply in progress there.
    pub fn peek(ctx: DeviceCtx, project_id: Uuid, root: &Path) -> Result<Engine> {
        Engine::load(ctx, project_id, root)
    }

    pub fn open(ctx: DeviceCtx, project_id: Uuid, root: &Path) -> Result<Engine> {
        let e = Engine::load(ctx, project_id, root)?;
        let marker = e.data_dir().join(APPLY_MARKER);
        if root.is_dir() && marker.exists() {
            let m = &e.matcher;
            platform::cleanup_tmp(root, &|p| {
                RelPath::from_fs(root, p).is_some_and(|r| !m.should_descend(&r))
            })?;
            std::fs::remove_file(&marker)?;
        }
        Ok(e)
    }

    /// `spoko init`: registers a new project on the server and pushes the first snapshot.
    pub fn create(ctx: DeviceCtx, root: &Path, name: &str) -> Result<Engine> {
        // validate .spokoconfig before registering anything on the server
        ProjectConfig::load(root).map_err(spoko_core::Error::from)?;
        let meta = spoko_crypto::seal(&ctx.keyring, b"project", name.as_bytes());
        let info = ctx.api.create_project(&meta)?;
        let mut e = Engine::load(ctx, info.id, root)?;
        match e.sync()? {
            SyncOutcome::Pushed => Ok(e),
            other => Err(ClientError::Other(format!(
                "initial push failed: {other:?}"
            ))),
        }
    }

    pub fn save(&mut self) -> Result<()> {
        self.state.save(&self.dir.join("state.json"))?;
        let due = self.statcache.entries.len() < STATCACHE_ALWAYS_SAVE
            || self.statcache_saved.is_none_or(|t| t.elapsed() >= STATCACHE_SAVE_EVERY);
        if due {
            self.statcache.save(&self.dir.join("statcache.bin"))?;
            self.statcache_saved = Some(std::time::Instant::now());
        } else {
            // entries recorded until now count as written now (racy-git window), as a save would
            self.statcache.mark_written();
        }
        for (name, c) in &mut self.ext_caches {
            c.save(&self.dir.join(format!("ext-{name}.cache")))?;
        }
        Ok(())
    }

    fn notify(&self, n: Notification) {
        self.ctx.notifier.notify(n);
    }

    fn reload_config(&mut self) -> Result<()> {
        self.cfg = ProjectConfig::load(&self.root).map_err(spoko_core::Error::from)?;
        self.matcher = Matcher::new(&self.root, &self.cfg).map_err(spoko_core::Error::from)?;
        self.externals = resolve_externals(&self.cfg, &self.root, &self.ctx.env);
        for x in &self.externals {
            if !self.ext_caches.contains_key(&x.name) {
                self.ext_caches.insert(
                    x.name.clone(),
                    StatCache::load(&self.dir.join(format!("ext-{}.cache", x.name)))?,
                );
            }
        }
        Ok(())
    }

    pub fn pause(&mut self) -> Result<()> {
        self.state.paused = Some(PauseReason::User);
        self.save()
    }

    /// Lifts a pause. A mass-delete pause needs `confirm` (the deletions will be pushed).
    pub fn resume(&mut self, confirm: bool) -> Result<()> {
        match &self.state.paused {
            Some(PauseReason::MassDelete { .. }) if !confirm => {
                return Err(ClientError::Other(
                    "mass delete pending: confirm to push the deletions".into(),
                ));
            }
            Some(PauseReason::MassDelete { .. }) => self.state.guard_override = true,
            Some(PauseReason::MissingRoot) if !self.root.is_dir() => {
                return Err(ClientError::Other(format!(
                    "project directory {} still missing",
                    self.root.display()
                )));
            }
            _ => {}
        }
        self.state.paused = None;
        self.save()
    }

    /// An automatic pause (the user's own goes through [`Engine::pause`]), with its notification.
    fn pause_for(&mut self, reason: PauseReason, n: Notification) -> Result<()> {
        self.state.paused = Some(reason);
        self.notify(n);
        self.save()
    }

    /// Captures the disk into a snapshot (child of `last_local`). Returns `last_local` itself
    /// when nothing changed, `None` when the project got paused.
    pub fn capture(&mut self, dirty: Option<&BTreeSet<RelPath>>) -> Result<Option<Oid>> {
        if !self.root.is_dir() {
            let n = Notification::MissingRoot { project: self.project_id };
            self.pause_for(PauseReason::MissingRoot, n)?;
            return Ok(None);
        }
        let dirty = dirty.filter(|d| !d.iter().any(is_config_path));
        if dirty.is_none() {
            self.reload_config()?;
        }
        // new file contents go into one packfile instead of a loose object each
        let scan = self.store.batch(|| -> Result<_> {
            Ok(match dirty {
                Some(d) => incremental_scan(
                    &self.root,
                    &self.matcher,
                    &self.store,
                    &mut self.statcache,
                    &self.work,
                    d,
                )?,
                None => full_scan_with(
                    &self.root,
                    &self.matcher,
                    &self.store,
                    &mut self.statcache,
                    &self.work,
                    None,
                )?,
            })
        })?;
        let mut work = scan.work;
        for (p, e) in &self.state.unmaterialized {
            if !work.contains_key(p) && !work.keys().any(|k| k.is_within(p)) {
                work.insert(p.clone(), *e);
            }
        }
        // paths that left the scan but are still on disk became ignored: not a deletion
        let mut guard_prev = WorkMap::default();
        for (p, e) in self.work.iter() {
            if work.contains_key(p) || read_entry(&p.to_fs(&self.root))?.is_none() {
                guard_prev.insert(p.clone(), *e);
            }
        }
        let now = self.now();
        let deletion = if self.state.guard_override {
            None
        } else {
            let (verdict, d) = self.state.deletions.check(now, &guard_prev, &work);
            if let GuardVerdict::MassDelete { deleted, total } = verdict {
                let n = Notification::MassDelete { project: self.project_id, deleted, total };
                self.pause_for(PauseReason::MassDelete { deleted, total }, n)?;
                return Ok(None);
            }
            d
        };

        let prev = self
            .state
            .last_local
            .map(|o| read_snapshot(&self.store, o))
            .transpose()?;
        let gitstate = self.capture_gitstate(prev.as_ref().and_then(|p| p.gitstate))?;

        let mut externals = prev
            .as_ref()
            .map(|p| p.externals.clone())
            .unwrap_or_default();
        let mut ext_work = self.ext_work.clone();
        for x in &self.externals {
            let w = if x.local_dir.is_dir() {
                let m = Matcher::new(&x.local_dir, &ProjectConfig::default())
                    .map_err(spoko_core::Error::from)?;
                let rw = x.rewrite_paths.then(|| PathRewrite {
                    root: self.root.to_string_lossy().into_owned(),
                });
                let cache = self
                    .ext_caches
                    .get_mut(&x.name)
                    .expect("cache per external");
                let empty = WorkMap::default();
                let prev_w = self.ext_work.get(&x.name).unwrap_or(&empty);
                full_scan_with(
                    &x.local_dir,
                    &m,
                    &self.store,
                    cache,
                    prev_w,
                    rw.as_ref().map(|r| r as _),
                )?
                .work
            } else {
                WorkMap::default()
            };
            if w.is_empty() {
                externals.remove(&x.name);
                ext_work.remove(&x.name);
            } else {
                externals.insert(x.name.clone(), self.store.build_tree(&w)?);
                ext_work.insert(x.name.clone(), w);
            }
        }

        let conflicts = carry_conflicts(
            prev.as_ref().map_or(&[][..], |p| &p.meta.conflicts),
            &[],
            &self.state.resolved,
        );
        let parts = SnapshotParts {
            work: self.store.build_tree(&work)?,
            gitstate,
            externals,
            meta: SnapshotMeta {
                device_id: self.ctx.device_id,
                device_name: self.ctx.device_name.clone(),
                time: now,
                conflicts,
            },
        };
        let unchanged = prev.as_ref().is_some_and(|p| {
            p.work == parts.work
                && p.gitstate == parts.gitstate
                && p.externals == parts.externals
                && p.meta.conflicts == parts.meta.conflicts
        });
        let oid = if unchanged {
            self.state.last_local.expect("prev exists")
        } else {
            let parents: Vec<Oid> = self.state.last_local.into_iter().collect();
            let oid = write_snapshot(&self.store, &parts, &parents)?;
            self.state.last_local = Some(oid);
            if std::mem::take(&mut self.state.guard_override) {
                self.state.deletions.clear();
            }
            oid
        };
        self.state.deletions.commit(now, &work, deletion);
        self.work = work;
        self.ext_work = ext_work;
        self.save()?;
        Ok(Some(oid))
    }

    /// The gitstate section for a new local snapshot; applies a deferred remote gitstate first.
    fn capture_gitstate(&mut self, prev: Option<Oid>) -> Result<Option<Oid>> {
        match gitstate::capture(&self.root, &self.store)? {
            CaptureOutcome::Captured(gs) => {
                let mut tree = gitstate_tree(&self.store, &gs)?;
                if let Some(pending) = self.state.pending_gitstate {
                    let target = if Some(tree) == self.state.applied_gitstate {
                        read_gitstate(&self.store, pending)?
                    } else {
                        // the user changed git meanwhile: merge their change with the pending one
                        let base = self
                            .state
                            .applied_gitstate
                            .map(|t| read_gitstate(&self.store, t))
                            .transpose()?
                            .flatten();
                        read_gitstate(&self.store, pending)?.map(|p| {
                            gitstate::merge_gitstate(
                                base.as_ref(),
                                &gs,
                                &p,
                                true,
                                &self.ctx.device_name,
                            )
                            .state
                        })
                    };
                    if let Some(target) = target {
                        self.git_hook();
                        match gitstate::apply(&self.root, &self.store, Some(&gs), &target)? {
                            ApplyGitOutcome::Applied => {
                                self.state.pending_gitstate = None;
                                tree = self.recapture_gitstate()?.unwrap_or(tree);
                            }
                            // busy again: like a busy capture, keep the previous state for now
                            ApplyGitOutcome::Deferred(_) => return Ok(prev),
                        }
                    }
                }
                self.state.applied_gitstate = Some(tree);
                Ok(Some(tree))
            }
            CaptureOutcome::NoRepo => Ok(None),
            CaptureOutcome::Busy(_) | CaptureOutcome::Unsupported(_) => Ok(prev),
        }
    }

    fn git_hook(&self) {
        if let Some(hook) = &self.before_git {
            hook();
        }
    }

    fn recapture_gitstate(&self) -> Result<Option<Oid>> {
        self.git_hook();
        Ok(match gitstate::capture(&self.root, &self.store)? {
            CaptureOutcome::Captured(gs) => Some(gitstate_tree(&self.store, &gs)?),
            _ => None,
        })
    }

    /// Makes the disk match snapshot `to`, starting from `last_local`. On success `last_local = to`.
    /// Marks the apply as in progress so a crash (or failure) half-way triggers the temp-file
    /// cleanup on the next open; a clean open never walks the tree for it.
    fn apply_snapshot(&mut self, to: Oid, join: bool) -> Result<Applied> {
        let marker = self.data_dir().join(APPLY_MARKER);
        std::fs::write(&marker, b"")?;
        let r = self.apply_snapshot_inner(to, join)?;
        std::fs::remove_file(&marker)?;
        Ok(r)
    }

    fn apply_snapshot_inner(&mut self, to: Oid, join: bool) -> Result<Applied> {
        let target = read_snapshot(&self.store, to)?;
        let current = self
            .state
            .last_local
            .map(|o| read_snapshot(&self.store, o))
            .transpose()?;
        if let Some(hook) = self.before_apply.take() {
            hook();
        }

        // work/
        let full_to = self.store.read_tree(target.work)?;
        let mut from_w = self.work.clone();
        let mut to_w = full_to.clone();
        let mut unmat = BTreeMap::new();
        for p in self.state.unmaterialized.keys() {
            from_w.remove(p);
            let Some(new) = to_w.get(p).copied() else {
                continue;
            };
            let retry = read_entry(&p.to_fs(&self.root))?.is_none()
                && !(cfg!(windows) && platform::windows_problem(p).is_some());
            if !retry {
                to_w.remove(p);
                unmat.insert(p.clone(), new);
            }
        }
        // Config first: the new ignore rules decide what a missing path means. A path the
        // remote dropped because it is now ignored is forgotten here, never deleted.
        let mut cfg_to = from_w.clone();
        for p in from_w.keys().chain(to_w.keys()).filter(|p| is_config_path(p)) {
            match to_w.get(p) {
                Some(e) => cfg_to.insert(p.clone(), *e),
                None => cfg_to.remove(p),
            };
        }
        if cfg_to != from_w {
            let opts = ApplyOpts { case_insensitive: self.case_insensitive, windows_names: cfg!(windows), faults: self.faults.as_deref().map(|f| f as _), transform: None };
            match apply_tree(&self.root, &self.store, &mut self.statcache, &from_w, &cfg_to, &opts) {
                Ok(_) => {}
                Err(ApplyError::LocalChanged(_)) => return Ok(Applied::LocalChanged),
                Err(ApplyError::Core(e)) => return Err(e.into()),
            }
            self.reload_config()?;
            from_w = cfg_to;
            let forgotten: Vec<RelPath> = from_w.keys().filter(|p| !to_w.contains_key(*p) && self.matcher.classify(p, false) == Class::Ignored).cloned().collect();
            for p in forgotten {
                from_w.remove(&p);
                self.statcache.entries.remove(&p);
            }
        }
        let changed: Vec<RelPath> = self.work
            .keys()
            .chain(full_to.keys())
            .filter(|p| self.work.get(*p) != full_to.get(*p))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let opts = ApplyOpts {
            case_insensitive: self.case_insensitive,
            windows_names: cfg!(windows),
            faults: self.faults.as_deref().map(|f| f as _),
            transform: None,
        };
        let report = match apply_tree(
            &self.root,
            &self.store,
            &mut self.statcache,
            &from_w,
            &to_w,
            &opts,
        ) {
            Ok(r) => r,
            Err(ApplyError::LocalChanged(_)) => return Ok(Applied::LocalChanged),
            Err(ApplyError::Core(e)) => return Err(e.into()),
        };
        for (p, reason) in &report.skipped {
            unmat.insert(p.clone(), to_w[p]);
            self.notify(Notification::PlatformSkip {
                project: self.project_id,
                path: p.to_string(),
                reason: reason.to_string(),
            });
        }
        // the work/ part is on disk now
        self.work = full_to;
        self.state.unmaterialized = unmat;
        if !join {
            self.queue_hooks(&changed);
        }

        // external/<name>/
        for x in self.externals.clone() {
            let empty = WorkMap::default();
            let from_e = self.ext_work.get(&x.name).unwrap_or(&empty).clone();
            let to_e = match target.externals.get(&x.name) {
                Some(t) => self.store.read_tree(*t)?,
                None => WorkMap::default(),
            };
            if from_e == to_e {
                continue;
            }
            std::fs::create_dir_all(&x.local_dir)?;
            let rw = x.rewrite_paths.then(|| PathRewrite {
                root: self.root.to_string_lossy().into_owned(),
            });
            let eopts = ApplyOpts {
                case_insensitive: self.case_insensitive,
                windows_names: cfg!(windows),
                faults: self.faults.as_deref().map(|f| f as _),
                transform: rw.as_ref().map(|r| r as _),
            };
            let cache = self
                .ext_caches
                .get_mut(&x.name)
                .expect("cache per external");
            match apply_tree(&x.local_dir, &self.store, cache, &from_e, &to_e, &eopts) {
                Ok(_) => {}
                Err(ApplyError::LocalChanged(_)) => return Ok(Applied::LocalChanged),
                Err(ApplyError::Core(e)) => return Err(e.into()),
            }
            self.ext_work.insert(x.name.clone(), to_e);
        }

        // gitstate/
        let cur_gs = current.as_ref().and_then(|c| c.gitstate);
        if let Some(tree) = target.gitstate
            && (target.gitstate != cur_gs || join)
            && let Some(mut want) = read_gitstate(&self.store, tree)?
        {
            match gitstate::capture(&self.root, &self.store)? {
                CaptureOutcome::Busy(_) | CaptureOutcome::Unsupported(_) => {
                    self.state.pending_gitstate = Some(tree);
                    self.notify(Notification::GitBusy {
                        project: self.project_id,
                    });
                }
                CaptureOutcome::Captured(gs) => {
                    if !join
                        && Some(gitstate_tree(&self.store, &gs)?) != self.state.applied_gitstate
                    {
                        return Ok(Applied::LocalChanged);
                    }
                    if join {
                        for (name, oid) in &gs.refs {
                            want.refs.entry(name.clone()).or_insert(*oid);
                        }
                    }
                    self.apply_git(Some(&gs), &want, tree)?;
                }
                CaptureOutcome::NoRepo => self.apply_git(None, &want, tree)?,
            }
        }

        self.state.last_local = Some(to);
        self.save()?;
        let hooks = std::mem::take(&mut self.fresh_hooks);
        if !hooks.is_empty() {
            crate::hooks::on_remote_change(self, hooks)?;
        }
        Ok(Applied::Done)
    }

    fn apply_git(&mut self, from: Option<&GitState>, want: &GitState, tree: Oid) -> Result<()> {
        self.git_hook();
        match gitstate::apply(&self.root, &self.store, from, want)? {
            ApplyGitOutcome::Applied => {
                self.state.pending_gitstate = None;
                self.state.applied_gitstate = self.recapture_gitstate()?;
            }
            ApplyGitOutcome::Deferred(_) => {
                self.state.pending_gitstate = Some(tree);
                self.notify(Notification::GitBusy {
                    project: self.project_id,
                });
            }
        }
        Ok(())
    }

    fn queue_hooks(&mut self, changed: &[RelPath]) {
        for p in changed {
            for h in self.matcher.watch_hooks_for(p) {
                if !self.fresh_hooks.contains(&h) {
                    self.fresh_hooks.push(h);
                }
            }
        }
    }

    /// Per-project data directory (`$SPOKO_HOME/projects/<id>`).
    pub fn data_dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn now(&self) -> i64 {
        (self.ctx.clock)()
    }

    /// Current time from the device clock (unix seconds).
    pub fn now_public(&self) -> i64 {
        self.now()
    }

    /// Number of object ids known to be on the server.
    pub fn known_objects(&self) -> usize {
        self.known.len()
    }

    pub fn config(&self) -> &ProjectConfig {
        &self.cfg
    }
}
