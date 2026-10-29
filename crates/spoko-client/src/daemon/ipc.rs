//! CLI/tray ↔ daemon protocol: one JSON request line, one JSON response line, over a local
//! socket (Unix domain socket / Windows named pipe) private to this `SPOKO_HOME`.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use interprocess::local_socket::prelude::*;
#[cfg(windows)]
use interprocess::local_socket::Name;
use interprocess::local_socket::{ListenerOptions, Stream};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::hooks::HookRun;
use crate::{ClientError, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum RestoreSpec {
    Commit { oid: String },
    Before { time: i64 },
    Join,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Status,
    Sync { project: Uuid },
    Pause { project: Uuid },
    Resume { project: Uuid, confirm: bool },
    Resolve { project: Uuid, path: String, done: bool },
    HooksRun { project: Uuid },
    HooksDismiss { project: Uuid },
    VolumePush { project: Uuid, volume: Option<String> },
    VolumePull { project: Uuid, volume: Option<String>, force: bool, fresh: bool },
    Log { project: Uuid, path: Option<String>, limit: usize },
    Restore { project: Uuid, path: String, from: RestoreSpec },
    /// The SSH key's passphrase, for a daemon that started locked.
    Unlock { passphrase: String },
    /// Projects were added or removed in `LocalConfig`: restart the workers.
    Reload,
    Shutdown,
}

impl Request {
    pub fn project(&self) -> Option<Uuid> {
        match self {
            Request::Status | Request::Unlock { .. } | Request::Reload | Request::Shutdown => None,
            Request::Sync { project }
            | Request::Pause { project }
            | Request::Resume { project, .. }
            | Request::Resolve { project, .. }
            | Request::HooksRun { project }
            | Request::HooksDismiss { project }
            | Request::VolumePush { project, .. }
            | Request::VolumePull { project, .. }
            | Request::Log { project, .. }
            | Request::Restore { project, .. } => Some(*project),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "snake_case")]
pub enum SyncState {
    Synced,
    Syncing,
    Conflict(usize),
    Offline,
    Paused(String),
}

/// "Pull `volume` from `from_device`?" after a device switch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumePrompt {
    pub volume: String,
    pub from_device: String,
    pub at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectStatus {
    pub id: Uuid,
    pub name: String,
    pub root: PathBuf,
    pub state: SyncState,
    pub last_sync: Option<i64>,
    pub pending_hooks: Vec<String>,
    pub conflicts: usize,
    pub warnings: Vec<String>,
    #[serde(default)]
    pub volume_prompts: Vec<VolumePrompt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusReport {
    pub device_name: String,
    pub projects: Vec<ProjectStatus>,
    /// Waiting for `spoko unlock` (the SSH key has a passphrase); nothing syncs meanwhile.
    #[serde(default)]
    pub locked: bool,
    /// Why nothing syncs when the daemon is up without a usable identity (not logged in, key
    /// missing or replaced); it waits instead of exiting, so autostart does not restart-loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogLine {
    pub commit: String,
    pub time: i64,
    pub device_name: String,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Error { message: String },
    Status { report: StatusReport },
    Text { text: String },
    Hooks { runs: Vec<HookRun> },
    Log { entries: Vec<LogLine> },
}

/// `$SPOKO_HOME` (default `~/.spoko`).
pub fn default_home() -> PathBuf {
    spoko_core::config::spoko_home()
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}

/// Socket address for a home. Unix: `<runtime dir>/spoko-<hash>/sock`, a short path (Unix
/// path length limits) in a directory only the owner of `SPOKO_HOME` may enter
/// (`$XDG_RUNTIME_DIR` when set, else the temp dir). Windows: a file in the home naming the pipe.
pub fn socket_path(home: &Path) -> PathBuf {
    if cfg!(windows) {
        return home.join("daemon.pipe");
    }
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| p.is_absolute()).unwrap_or_else(std::env::temp_dir);
    base.join(format!("spoko-{:016x}", fnv(&home.to_string_lossy()))).join("sock")
}

/// A passphrase crosses the socket: its directory must belong to the owner of `SPOKO_HOME`
/// and be closed to everyone else, or another local user could stand in for the daemon.
/// `repair`: the daemon creates the directory, or closes one it owns.
#[cfg(unix)]
fn private_dir(home: &Path, repair: bool) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let dir = socket_path(home).parent().expect("socket in a directory").to_path_buf();
    if repair {
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(e),
            _ => {}
        }
    }
    let owner = std::fs::metadata(home)?.uid();
    let md = std::fs::symlink_metadata(&dir)?;
    let unsafe_dir = |why: &str| std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("daemon socket directory {} is not private ({why})", dir.display()));
    if !md.is_dir() || md.uid() != owner {
        return Err(unsafe_dir("owned by another user"));
    }
    if md.mode() & 0o077 != 0 {
        if !repair {
            return Err(unsafe_dir("open to other users"));
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(windows)]
fn pipe_name(n: &str) -> std::io::Result<Name<'static>> {
    use interprocess::local_socket::GenericNamespaced;
    n.to_string().to_ns_name::<GenericNamespaced>()
}

pub fn listen(home: &Path) -> Result<interprocess::local_socket::Listener> {
    #[cfg(unix)]
    let listener = {
        use interprocess::local_socket::GenericFilePath;
        use std::os::unix::fs::PermissionsExt;
        private_dir(home, true)?;
        let path = socket_path(home);
        // the daemon lock guarantees a single owner, so a leftover socket file is stale
        let l = ListenerOptions::new().name(path.clone().to_fs_name::<GenericFilePath>()?).try_overwrite(true).create_sync()?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        l
    };
    // a pipe name nobody can guess (pipes live in one global namespace); the home is the user's
    #[cfg(windows)]
    let listener = {
        let n = format!("spoko-{:032x}", rand::random::<u128>());
        let l = ListenerOptions::new().name(pipe_name(&n)?).create_sync()?;
        std::fs::write(socket_path(home), &n)?;
        l
    };
    Ok(listener)
}

fn connect(home: &Path) -> std::io::Result<Stream> {
    #[cfg(unix)]
    let name = {
        use interprocess::local_socket::GenericFilePath;
        private_dir(home, false)?;
        socket_path(home).to_fs_name::<GenericFilePath>()?
    };
    #[cfg(windows)]
    let name = pipe_name(std::fs::read_to_string(socket_path(home))?.trim())?;
    Stream::connect(name)
}

/// Serves connections until a `Shutdown` request.
pub fn serve(listener: &interprocess::local_socket::Listener, handle: &mut dyn FnMut(Request) -> Response) {
    serve_until(listener, &mut |req| (handle(req), false));
}

/// Like [`serve`], but also stops after a request for which `handle` says so.
pub fn serve_until(listener: &interprocess::local_socket::Listener, handle: &mut dyn FnMut(Request) -> (Response, bool)) {
    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        if serve_one(conn, handle).unwrap_or(false) {
            return;
        }
    }
}

/// Reads one request from a connection and writes `handle`'s response; `true` = stop serving.
pub fn serve_one(conn: Stream, handle: &mut dyn FnMut(Request) -> (Response, bool)) -> Result<bool> {
    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let (resp, shutdown) = match serde_json::from_str::<Request>(&line) {
        Ok(Request::Shutdown) => (Response::Ok, true),
        Ok(req) => handle(req),
        Err(e) => (Response::Error { message: format!("bad request: {e}") }, false),
    };
    let mut out = serde_json::to_vec(&resp).expect("response serializes");
    out.push(b'\n');
    reader.get_mut().write_all(&out)?;
    Ok(shutdown)
}

/// Sends one request to the running daemon.
pub fn request(home: &Path, req: &Request) -> Result<Response> {
    let conn = connect(home).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => ClientError::Other(e.to_string()),
        _ => ClientError::Other(format!("daemon not running ({e})")),
    })?;
    let mut line = serde_json::to_vec(req).expect("request serializes");
    line.push(b'\n');
    let mut reader = BufReader::new(conn);
    reader.get_mut().write_all(&line)?;
    let mut resp = String::new();
    reader.read_line(&mut resp)?;
    serde_json::from_str(&resp).map_err(|e| ClientError::Other(format!("bad daemon response: {e}")))
}
