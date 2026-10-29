//! Command implementations. Project commands go through the daemon when it runs, otherwise
//! they run in this process under the project's lock.

pub mod account;
pub mod config;
pub mod daemon;
pub mod project;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use spoko_client::account::Identity;
use spoko_client::daemon::ipc::{self, Request, Response};
use spoko_client::engine::{DeviceCtx, Engine};
use spoko_client::notifier::{Notification, Notifier};
use spoko_core::config::{LocalConfig, spoko_home};
use spoko_core::external::ExternalEnv;
use uuid::Uuid;

use crate::cli::Cmd;

pub type Res<T> = Result<T, Box<dyn std::error::Error>>;

pub fn err<T>(msg: impl Into<String>) -> Res<T> {
    Err(msg.into().into())
}

/// Notifications in a terminal go to stderr.
pub struct PrintNotifier;

impl Notifier for PrintNotifier {
    fn notify(&self, n: Notification) {
        eprintln!("spoko: {}", n.text());
    }
}

pub struct Ctx {
    pub home: PathBuf,
    pub env: ExternalEnv,
    pub cwd: PathBuf,
}

impl Ctx {
    pub fn from_env() -> Ctx {
        Ctx { home: spoko_home(), env: ExternalEnv::from_process(), cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")) }
    }

    pub fn identity(&self) -> Res<Identity> {
        if !self.home.join(spoko_client::account::IDENTITY_FILE).exists() {
            return err("ten komputer nie jest zalogowany (spoko login <serwer>)");
        }
        Ok(Identity::load(&self.home, None)?)
    }

    pub fn device_ctx(&self, id: &Identity) -> DeviceCtx {
        DeviceCtx::new(&self.home, id, self.env.clone(), Arc::new(PrintNotifier))
    }

    pub fn local(&self) -> Res<LocalConfig> {
        Ok(LocalConfig::load(&self.home)?)
    }

    /// The registered project containing the current directory.
    pub fn current_project(&self) -> Res<(Uuid, PathBuf)> {
        let cwd = self.cwd.canonicalize().unwrap_or_else(|_| self.cwd.clone());
        let local = self.local()?;
        local
            .projects
            .iter()
            .filter(|(_, p)| cwd.starts_with(&p.path))
            .max_by_key(|(_, p)| p.path.as_os_str().len())
            .map(|(id, p)| (*id, p.path.clone()))
            .ok_or_else(|| format!("{} nie jest w żadnym projekcie spoko (spoko init / spoko clone)", self.cwd.display()).into())
    }

    pub fn daemon_running(&self) -> bool {
        ipc::request(&self.home, &Request::Status).is_ok()
    }

    /// Sends a request to the running daemon; `None` when no daemon runs.
    pub fn via_daemon(&self, req: &Request) -> Res<Option<Response>> {
        if !self.daemon_running() {
            return Ok(None);
        }
        match ipc::request(&self.home, req)? {
            Response::Error { message } => err(message),
            r => Ok(Some(r)),
        }
    }

    /// Runs `f` on the project's engine in this process, holding the project lock.
    pub fn with_engine<T>(&self, project: Uuid, root: &Path, f: impl FnOnce(&mut Engine) -> Res<T>) -> Res<T> {
        let id = self.identity()?;
        let dir = spoko_client::engine::project_dir(&self.home, project);
        std::fs::create_dir_all(&dir)?;
        let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join("lock"))?;
        let mut lock = fd_lock::RwLock::new(file);
        let _g = lock.try_write().map_err(|_| "projekt jest właśnie używany przez inny proces spoko")?;
        let mut e = Engine::open(self.device_ctx(&id), project, root)?;
        f(&mut e)
    }

    pub fn register(&self, project: Uuid, root: &Path) -> Res<()> {
        let mut local = self.local()?;
        local.projects.insert(project, spoko_core::config::LocalProject { path: root.to_path_buf(), paused: false });
        local.save(&self.home)?;
        Ok(())
    }
}

pub fn run(ctx: &mut Ctx, cmd: Cmd, out: &mut dyn Write) -> Res<()> {
    match cmd {
        Cmd::Login { server, key, recovery_key, name } => account::login(ctx, &server, key, recovery_key.as_deref(), name, out),
        Cmd::Device { cmd } => account::device(ctx, cmd, out),
        Cmd::Init { path, name } => project::init(ctx, path, name, out),
        Cmd::Clone { project, path } => project::clone(ctx, &project, path, out),
        Cmd::Sync => project::sync(ctx, out),
        Cmd::Status { json } => project::status(ctx, json, out),
        Cmd::Resolve { path, done } => project::resolve(ctx, path, done, out),
        Cmd::Log { path, limit, json } => project::log(ctx, path, limit, json, out),
        Cmd::Restore { path, at, from_join } => project::restore(ctx, &path, at.as_deref(), from_join, out),
        Cmd::Pause => project::pause(ctx, out),
        Cmd::Resume { confirm } => project::resume(ctx, confirm, out),
        Cmd::Volume { cmd } => project::volume(ctx, cmd, out),
        Cmd::Hooks { cmd } => project::hooks(ctx, cmd, out),
        Cmd::Config { cmd: crate::cli::ConfigCmd::Check } => config::check(ctx, out),
        Cmd::Daemon { cmd } => daemon::run(ctx, cmd, out),
    }
}

/// `2026-10-29 13:05:00 UTC` from unix seconds.
pub fn fmt_time(t: i64) -> String {
    let (days, secs) = (t.div_euclid(86_400), t.rem_euclid(86_400));
    // civil-from-days (H. Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC", secs / 3600, secs % 3600 / 60, secs % 60)
}

pub fn fmt_size(b: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{b} B") } else { format!("{v:.1} {}", U[i]) }
}
