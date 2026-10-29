//! CLI/tray ↔ daemon protocol: one JSON request line, one JSON response line, over a local
//! socket (Unix domain socket / Windows named pipe) private to this `SPOKO_HOME`.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericFilePath, GenericNamespaced, ListenerOptions, Name, Stream};
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
    VolumePush { project: Uuid, volume: Option<String> },
    VolumePull { project: Uuid, volume: Option<String>, force: bool, fresh: bool },
    Log { project: Uuid, path: Option<String>, limit: usize },
    Restore { project: Uuid, path: String, from: RestoreSpec },
    Shutdown,
}

impl Request {
    pub fn project(&self) -> Option<Uuid> {
        match self {
            Request::Status | Request::Shutdown => None,
            Request::Sync { project }
            | Request::Pause { project }
            | Request::Resume { project, .. }
            | Request::Resolve { project, .. }
            | Request::HooksRun { project }
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

/// Socket address for a home: a short path in the temp dir (Unix path length limits) or a
/// namespaced pipe on Windows.
pub fn socket_path(home: &Path) -> PathBuf {
    std::env::temp_dir().join(format!("spoko-{:016x}.sock", fnv(&home.to_string_lossy())))
}

fn name(home: &Path) -> std::io::Result<Name<'static>> {
    if cfg!(windows) {
        format!("spoko-{:016x}", fnv(&home.to_string_lossy())).to_ns_name::<GenericNamespaced>()
    } else {
        socket_path(home).to_fs_name::<GenericFilePath>()
    }
}

pub fn listen(home: &Path) -> Result<interprocess::local_socket::Listener> {
    // the daemon lock guarantees a single owner, so a leftover socket file is stale
    Ok(ListenerOptions::new().name(name(home)?).try_overwrite(true).create_sync()?)
}

/// Serves connections until a `Shutdown` request.
pub fn serve(listener: &interprocess::local_socket::Listener, handle: &mut dyn FnMut(Request) -> Response) {
    for conn in listener.incoming() {
        let Ok(conn) = conn else { continue };
        if serve_one(conn, handle).unwrap_or(false) {
            return;
        }
    }
}

/// Reads one request from a connection and writes `handle`'s response.
pub fn serve_one(conn: Stream, handle: &mut dyn FnMut(Request) -> Response) -> Result<bool> {
    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let (resp, shutdown) = match serde_json::from_str::<Request>(&line) {
        Ok(Request::Shutdown) => (Response::Ok, true),
        Ok(req) => (handle(req), false),
        Err(e) => (Response::Error { message: format!("bad request: {e}") }, false),
    };
    let mut out = serde_json::to_vec(&resp).expect("response serializes");
    out.push(b'\n');
    reader.get_mut().write_all(&out)?;
    Ok(shutdown)
}

/// Sends one request to the running daemon.
pub fn request(home: &Path, req: &Request) -> Result<Response> {
    let conn = Stream::connect(name(home)?).map_err(|e| ClientError::Other(format!("daemon not running ({e})")))?;
    let mut line = serde_json::to_vec(req).expect("request serializes");
    line.push(b'\n');
    let mut reader = BufReader::new(conn);
    reader.get_mut().write_all(&line)?;
    let mut resp = String::new();
    reader.read_line(&mut resp)?;
    serde_json::from_str(&resp).map_err(|e| ClientError::Other(format!("bad daemon response: {e}")))
}
