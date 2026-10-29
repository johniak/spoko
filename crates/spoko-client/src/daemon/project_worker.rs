//! One thread per project owning its [`Engine`]: coalesces file events, syncs, backs off
//! while the server is unreachable (local snapshots continue), answers IPC requests.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use spoko_core::platform::RelPath;

use super::Extensions;
use super::ipc::{LogLine, ProjectStatus, Request, Response, RestoreSpec, SyncState};
use super::watcher::FsBatch;
use crate::ClientError;
use crate::engine::{Engine, PauseReason};
use crate::history::{self, RestoreFrom};
use crate::resolve::{self, MergeTool, ResolveOutcome};

pub enum WorkerMsg {
    Fs(FsBatch),
    /// The project's head moved on the server (or we reconnected).
    Remote,
    Rescan,
    /// An encrypted project message (e.g. a volume request).
    Message(Vec<u8>),
    Ipc(Request, Sender<Response>),
    Shutdown,
}

pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);

pub struct Worker {
    pub engine: Engine,
    pub ext: Arc<dyn Extensions>,
    pub status: Arc<Mutex<ProjectStatus>>,
    pub rescan_every: Duration,
    dirty: BTreeSet<RelPath>,
    full: bool,
    need_sync: bool,
    local_activity: bool,
    backoff: Option<(Duration, Instant)>,
    next_rescan: Instant,
}

fn is_network(e: &ClientError) -> bool {
    matches!(e, ClientError::Network(_) | ClientError::Server(_))
}

impl Worker {
    pub fn new(engine: Engine, ext: Arc<dyn Extensions>, status: Arc<Mutex<ProjectStatus>>, rescan_every: Duration) -> Worker {
        Worker {
            engine,
            ext,
            status,
            rescan_every,
            dirty: BTreeSet::new(),
            full: true,
            need_sync: true,
            local_activity: false,
            backoff: None,
            next_rescan: Instant::now() + rescan_every,
        }
    }

    pub fn run(mut self, rx: &Receiver<WorkerMsg>) {
        loop {
            let wake = match self.backoff {
                Some((_, until)) if self.need_sync => until.min(self.next_rescan),
                _ if self.need_sync => Instant::now(),
                _ => self.next_rescan,
            };
            let msg = match rx.recv_timeout(wake.saturating_duration_since(Instant::now())) {
                Ok(m) => Some(m),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            let mut msgs: Vec<WorkerMsg> = msg.into_iter().collect();
            msgs.extend(rx.try_iter());
            for m in msgs {
                if !self.handle(m) {
                    return;
                }
            }
            if Instant::now() >= self.next_rescan {
                self.full = true;
                self.need_sync = true;
                self.next_rescan = Instant::now() + self.rescan_every;
            }
            if self.need_sync && self.backoff.is_none_or(|(_, until)| Instant::now() >= until) {
                self.sync_now();
            }
        }
    }

    /// `false` = stop.
    fn handle(&mut self, m: WorkerMsg) -> bool {
        match m {
            WorkerMsg::Fs(b) => {
                self.full |= b.rescan;
                self.dirty.extend(b.paths);
                self.need_sync = true;
                self.local_activity = true;
            }
            WorkerMsg::Remote => {
                self.need_sync = true;
                // the server is reachable again: retry right away
                self.backoff = self.backoff.map(|(d, _)| (d, Instant::now()));
            }
            WorkerMsg::Rescan => {
                self.full = true;
                self.need_sync = true;
            }
            WorkerMsg::Message(payload) => self.ext.on_message(&mut self.engine, &payload),
            WorkerMsg::Ipc(req, reply) => {
                let resp = self.ipc(req);
                let _ = reply.send(resp);
            }
            WorkerMsg::Shutdown => return false,
        }
        true
    }

    fn set_state(&self, state: SyncState) {
        let mut st = self.status.lock().expect("status");
        st.state = state;
    }

    fn sync_now(&mut self) -> Option<crate::Result<crate::engine::SyncOutcome>> {
        if self.local_activity {
            self.local_activity = false;
            self.ext.on_local_activity(&mut self.engine);
        }
        self.set_state(SyncState::Syncing);
        let dirty = std::mem::take(&mut self.dirty);
        let r = if self.full { self.engine.sync() } else { self.engine.sync_dirty(Some(&dirty)) };
        self.full = false;
        self.need_sync = false;
        match &r {
            Ok(_) => self.backoff = None,
            Err(e) => {
                // the capture may not have happened: rescan on retry
                self.full = true;
                self.need_sync = true;
                let next = self.backoff.map_or(BACKOFF_MIN, |(d, _)| (d * 2).min(BACKOFF_MAX));
                self.backoff = Some((next, Instant::now() + next));
                if !is_network(e) {
                    self.status.lock().expect("status").warnings = vec![e.to_string()];
                }
            }
        }
        self.refresh_status(r.as_ref().err());
        Some(r)
    }

    fn refresh_status(&self, err: Option<&ClientError>) {
        let conflicts = resolve::list(&self.engine).map(|c| c.len()).unwrap_or(0);
        let mut st = self.status.lock().expect("status");
        st.conflicts = conflicts;
        st.pending_hooks = self.engine.state.pending_hooks.iter().filter_map(|h| self.engine.config().watch.get(*h)).filter_map(|w| crate::hooks::command_for(w, crate::hooks::Os::current())).map(str::to_owned).collect();
        st.state = match (&self.engine.state.paused, err) {
            (Some(p), _) => SyncState::Paused(match p {
                PauseReason::User => "paused".into(),
                PauseReason::MassDelete { deleted, total } => format!("mass delete {deleted}/{total}"),
                PauseReason::MissingRoot => "project directory missing".into(),
            }),
            (None, Some(_)) => SyncState::Offline,
            (None, None) if conflicts > 0 => SyncState::Conflict(conflicts),
            (None, None) => SyncState::Synced,
        };
        if err.is_none() {
            st.last_sync = Some(self.engine.now());
            st.warnings.clear();
        }
    }

    fn ipc(&mut self, req: Request) -> Response {
        let res = self.ipc_inner(&req);
        self.refresh_status(None);
        res.unwrap_or_else(|err| Response::Error { message: err.to_string() })
    }

    fn ipc_inner(&mut self, req: &Request) -> crate::Result<Response> {
        match req {
            Request::Sync { .. } => {
                self.full = true;
                match self.sync_now() {
                    Some(Err(err)) => Err(err),
                    _ => Ok(Response::Ok),
                }
            }
            Request::Pause { .. } => self.engine.pause().map(|()| Response::Ok),
            Request::Resume { confirm, .. } => {
                self.engine.resume(*confirm)?;
                self.need_sync = true;
                Ok(Response::Ok)
            }
            Request::Resolve { path, done: true, .. } => resolve::mark_resolved(&mut self.engine, &RelPath::new(path)).map(|()| Response::Ok),
            Request::Resolve { path, done: false, .. } => {
                let tool = MergeTool::from_git_config(&self.engine.root);
                let out = resolve::resolve(&mut self.engine, &RelPath::new(path), &tool)?;
                Ok(Response::Text { text: if out == ResolveOutcome::Resolved { "resolved".into() } else { "unchanged".into() } })
            }
            Request::HooksRun { .. } => crate::hooks::run_pending(&mut self.engine).map(|runs| Response::Hooks { runs }),
            Request::Log { path, limit, .. } => {
                let p = path.as_deref().map(RelPath::new);
                let entries = history::log(&self.engine, p.as_ref(), *limit)?
                    .into_iter()
                    .map(|l| LogLine { commit: l.commit.to_string(), time: l.time, device_name: l.device_name, changed: l.changed })
                    .collect();
                Ok(Response::Log { entries })
            }
            Request::Restore { path, from, .. } => {
                let from = match from {
                    RestoreSpec::Commit { oid } => RestoreFrom::Commit(git2::Oid::from_str(oid)?),
                    RestoreSpec::Before { time } => RestoreFrom::Before(*time),
                    RestoreSpec::Join => RestoreFrom::Join,
                };
                history::restore(&mut self.engine, &RelPath::new(path), from)?;
                self.need_sync = true;
                Ok(Response::Ok)
            }
            Request::VolumePush { .. } | Request::VolumePull { .. } => {
                Ok(self.ext.handle(&mut self.engine, req).unwrap_or_else(|| Response::Error { message: "this build has no Docker support".into() }))
            }
            Request::Status | Request::Shutdown => Ok(Response::Ok),
        }
    }
}
