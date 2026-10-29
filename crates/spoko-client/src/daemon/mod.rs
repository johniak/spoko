//! The background process: one worker per project, a file watcher, the server's event
//! stream and a local IPC socket for the CLI and the tray.

pub mod autostart;
pub mod ipc;
pub mod project_worker;
pub mod watcher;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{Sender, bounded, unbounded};
use spoko_core::config::{LocalConfig, ProjectConfig};
use spoko_core::external::{ExternalEnv, resolve_externals};
use spoko_proto::ServerEvent;
use uuid::Uuid;

use self::ipc::{ProjectStatus, Request, Response, StatusReport, SyncState};
use self::project_worker::{Worker, WorkerMsg};
use crate::account::Identity;
use crate::engine::{DeviceCtx, Engine};
use crate::notifier::Notifier;
use crate::ws::EventStream;
use crate::{ClientError, Result};

/// Hooks for features that live in other crates (Docker volumes).
pub trait Extensions: Send + Sync {
    /// A project message arrived (e.g. another device asks for a volume snapshot).
    fn on_message(&self, _e: &mut Engine, _payload: &[u8]) {}
    /// Local changes are about to be synced (device-switch detection).
    fn on_local_activity(&self, _e: &mut Engine) {}
    /// Requests this crate cannot answer (`VolumePush`/`VolumePull`).
    fn handle(&self, _e: &mut Engine, _req: &Request) -> Option<Response> {
        None
    }
}

pub struct NoExtensions;
impl Extensions for NoExtensions {}

pub struct DaemonOpts {
    pub ext: Arc<dyn Extensions>,
    pub notifier: Arc<dyn Notifier>,
    pub env: ExternalEnv,
    pub rescan_every: Duration,
}

struct ProjectHandle {
    tx: Sender<WorkerMsg>,
    status: Arc<Mutex<ProjectStatus>>,
    thread: std::thread::JoinHandle<()>,
}

/// Runs until a `Shutdown` request. Fails if another daemon owns this `SPOKO_HOME`.
pub fn run(home: &Path, opts: DaemonOpts) -> Result<()> {
    // started before the first login (autostart): the home does not exist yet
    std::fs::create_dir_all(home)?;
    let lock_file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(home.join("daemon.lock"))?;
    let mut lock = fd_lock::RwLock::new(lock_file);
    let _guard = lock.try_write().map_err(|_| ClientError::Other("spoko daemon is already running for this SPOKO_HOME".into()))?;
    let local = LocalConfig::load(home).map_err(spoko_core::Error::from)?;
    let listener = ipc::listen(home)?;
    let identity = match Identity::load(home, None) {
        Ok(id) => id,
        // a key with a passphrase: wait for `spoko unlock`; anything else: say why and wait
        // for `spoko login` (which restarts the daemon) instead of exiting into a restart loop
        Err(e) => {
            let problem = match e {
                ClientError::Crypto(spoko_crypto::CryptoError::Encrypted) => None,
                e => Some(format!("{e} (spoko login)")),
            };
            match wait_for_unlock(home, &listener, &local, problem) {
                Some(id) => id,
                None => return remove_socket(home),
            }
        }
    };
    let prompts = Arc::new(PromptTracker { inner: opts.notifier.clone(), prompts: Mutex::new(BTreeMap::new()) });
    let ctx = DeviceCtx::new(home, &identity, opts.env.clone(), prompts.clone());
    // `Reload` (a project added or removed) restarts the workers, not the process: the
    // unlocked key stays in memory
    while serve_projects(home, &identity, &ctx, &listener, &opts, &prompts)? {}
    remove_socket(home)
}

/// Runs the projects of the current `LocalConfig` until `Shutdown` (`false`) or `Reload` (`true`).
fn serve_projects(home: &Path, identity: &Identity, ctx: &DeviceCtx, listener: &interprocess::local_socket::Listener, opts: &DaemonOpts, prompts: &PromptTracker) -> Result<bool> {
    let local = LocalConfig::load(home).map_err(spoko_core::Error::from)?;
    let mut projects: BTreeMap<Uuid, ProjectHandle> = BTreeMap::new();
    let mut roots: BTreeMap<PathBuf, Uuid> = BTreeMap::new();
    for (id, lp) in &local.projects {
        let engine = Engine::open(ctx.clone(), *id, &lp.path)?;
        let cfg = ProjectConfig::load(&lp.path).unwrap_or_default();
        roots.insert(lp.path.clone(), *id);
        for x in resolve_externals(&cfg, &lp.path, &opts.env) {
            roots.insert(x.local_dir, *id);
        }
        let name = lp.path.file_name().map_or_else(|| id.to_string(), |n| n.to_string_lossy().into_owned());
        let status = Arc::new(Mutex::new(ProjectStatus {
            id: *id,
            name,
            root: lp.path.clone(),
            state: SyncState::Syncing,
            last_sync: None,
            pending_hooks: vec![],
            conflicts: 0,
            warnings: vec![],
            volume_prompts: vec![],
        }));
        let (tx, rx) = unbounded();
        let worker = Worker::new(engine, opts.ext.clone(), status.clone(), opts.rescan_every);
        let thread = std::thread::Builder::new().name(format!("spoko-{id}")).spawn(move || worker.run(&rx))?;
        projects.insert(*id, ProjectHandle { tx, status, thread });
    }

    let stop = Arc::new(AtomicBool::new(false));
    let senders: BTreeMap<Uuid, Sender<WorkerMsg>> = projects.iter().map(|(id, p)| (*id, p.tx.clone())).collect();

    // file events → workers
    let (watcher, fs_rx) = watcher::Watcher::new(roots.keys().cloned().collect())?;
    let fs_thread = {
        let senders = senders.clone();
        std::thread::spawn(move || {
            for batch in fs_rx {
                if let Some(tx) = roots.get(&batch.root).and_then(|id| senders.get(id)) {
                    let _ = tx.send(WorkerMsg::Fs(batch));
                }
            }
        })
    };

    // server events → workers
    let ws_thread = {
        let (senders, stop) = (senders.clone(), stop.clone());
        let head = spoko_crypto::name_id(&identity.keyring, "ptr", "head");
        let (url, token) = (identity.server_url.clone(), identity.token.clone());
        std::thread::spawn(move || event_loop(&url, &token, &head, &senders, &stop))
    };

    let mut reload = false;
    ipc::serve_until(listener, &mut |req| match req {
        Request::Reload => {
            reload = true;
            (Response::Ok, true)
        }
        req => (dispatch(identity, &projects, prompts, req), false),
    });

    stop.store(true, Ordering::SeqCst);
    drop(watcher);
    for p in projects.values() {
        let _ = p.tx.send(WorkerMsg::Shutdown);
    }
    drop(senders);
    let _ = ws_thread.join();
    for (_, p) in projects {
        drop(p.tx);
        let _ = p.thread.join();
    }
    let _ = fs_thread.join();
    Ok(reload)
}

fn remove_socket(home: &Path) -> Result<()> {
    let path = ipc::socket_path(home);
    let _ = std::fs::remove_file(&path);
    #[cfg(unix)]
    let _ = path.parent().map(std::fs::remove_dir);
    Ok(())
}

const LOCKED: &str = "daemon is locked: the SSH key has a passphrase (run spoko unlock)";

/// Answers status requests until a correct passphrase arrives (`None`: shut down meanwhile).
/// `problem`: the identity is unusable for another reason; status reports it.
fn wait_for_unlock(home: &Path, listener: &interprocess::local_socket::Listener, local: &LocalConfig, problem: Option<String>) -> Option<Identity> {
    let mut unlocked = None;
    let device_name = local.device_name.clone().unwrap_or_default();
    let refusal = problem.clone().unwrap_or_else(|| LOCKED.into());
    ipc::serve_until(listener, &mut |req| match req {
        Request::Unlock { passphrase } => match Identity::load(home, Some(&passphrase)) {
            Ok(id) => {
                unlocked = Some(id);
                (Response::Ok, true)
            }
            Err(e) => (Response::Error { message: e.to_string() }, false),
        },
        Request::Status => {
            let report = StatusReport { device_name: device_name.clone(), projects: vec![], locked: problem.is_none(), problem: problem.clone() };
            (Response::Status { report }, false)
        }
        _ => (Response::Error { message: refusal.clone() }, false),
    });
    unlocked
}

/// Passes notifications on and remembers volume prompts for the status report (tray menu).
struct PromptTracker {
    inner: Arc<dyn Notifier>,
    prompts: Mutex<BTreeMap<Uuid, Vec<ipc::VolumePrompt>>>,
}

impl Notifier for PromptTracker {
    fn notify(&self, n: crate::notifier::Notification) {
        if let crate::notifier::Notification::VolumePrompt { project, volume, from_device, at } = &n {
            let mut map = self.prompts.lock().expect("prompts");
            let list = map.entry(*project).or_default();
            list.retain(|p| &p.volume != volume);
            list.push(ipc::VolumePrompt { volume: volume.clone(), from_device: from_device.clone(), at: *at });
        }
        self.inner.notify(n);
    }
}

fn dispatch(identity: &Identity, projects: &BTreeMap<Uuid, ProjectHandle>, prompts: &PromptTracker, req: Request) -> Response {
    if req == Request::Status {
        let map = prompts.prompts.lock().expect("prompts");
        let projects = projects
            .iter()
            .map(|(id, p)| {
                let mut st = p.status.lock().expect("status").clone();
                st.volume_prompts = map.get(id).cloned().unwrap_or_default();
                st
            })
            .collect();
        return Response::Status { report: StatusReport { device_name: identity.device_name.clone(), projects, locked: false, problem: None } };
    }
    if let Request::Unlock { .. } = req {
        return Response::Ok;
    }
    if let Request::VolumePull { project, volume, .. } = &req
        && let Some(list) = prompts.prompts.lock().expect("prompts").get_mut(project)
    {
        list.retain(|p| volume.as_ref().is_some_and(|v| v != &p.volume));
    }
    let Some(p) = req.project().and_then(|id| projects.get(&id)) else { return Response::Error { message: "unknown project".into() } };
    let (tx, rx) = bounded(1);
    if p.tx.send(WorkerMsg::Ipc(req, tx)).is_err() {
        return Response::Error { message: "project worker stopped".into() };
    }
    rx.recv().unwrap_or(Response::Error { message: "project worker stopped".into() })
}

fn event_loop(url: &str, token: &str, head: &str, senders: &BTreeMap<Uuid, Sender<WorkerMsg>>, stop: &AtomicBool) {
    let mut backoff = project_worker::BACKOFF_MIN;
    while !stop.load(Ordering::SeqCst) {
        let mut ws = match EventStream::connect(url, token) {
            Ok(ws) => ws,
            Err(_) => {
                sleep_unless(stop, backoff);
                backoff = (backoff * 2).min(project_worker::BACKOFF_MAX);
                continue;
            }
        };
        backoff = project_worker::BACKOFF_MIN;
        // (re)connected: anything may have happened meanwhile
        for tx in senders.values() {
            let _ = tx.send(WorkerMsg::Remote);
        }
        while !stop.load(Ordering::SeqCst) {
            match ws.next(Duration::from_millis(300)) {
                Ok(Some(ServerEvent::PointerChanged { project, name, .. })) if name == head => {
                    if let Some(tx) = senders.get(&project) {
                        let _ = tx.send(WorkerMsg::Remote);
                    }
                }
                Ok(Some(ServerEvent::Message { project, payload, .. })) => {
                    if let Some(tx) = senders.get(&project) {
                        let _ = tx.send(WorkerMsg::Message(payload));
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    }
}

fn sleep_unless(stop: &AtomicBool, d: Duration) {
    let end = std::time::Instant::now() + d;
    while !stop.load(Ordering::SeqCst) && std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
}
