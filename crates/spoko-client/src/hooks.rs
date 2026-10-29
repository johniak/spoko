//! `watch` hooks: commands to run after files they watch changed on another machine
//! (e.g. `npm ci` when `package-lock.json` changed).

use std::io::Write;
use std::process::Command;

use spoko_core::config::{HookMode, WatchCfg};

use crate::Result;
use crate::engine::Engine;
use crate::notifier::Notification;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Macos,
    Linux,
    Windows,
}

impl Os {
    pub fn current() -> Os {
        if cfg!(target_os = "macos") {
            Os::Macos
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Linux
        }
    }
}

/// `run_<os>` wins over `run`.
pub fn command_for(w: &WatchCfg, os: Os) -> Option<&str> {
    let specific = match os {
        Os::Macos => &w.run_macos,
        Os::Linux => &w.run_linux,
        Os::Windows => &w.run_windows,
    };
    specific.as_deref().or(w.run.as_deref())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRun {
    pub command: String,
    pub status: i32,
    pub output_tail: String,
}

const TAIL_BYTES: usize = 4000;

/// Called by the engine after an apply from the network changed watched files.
pub fn on_remote_change(e: &mut Engine, hooks: Vec<usize>) -> Result<()> {
    let mut changed = false;
    for h in hooks {
        let Some(w) = e.config().watch.get(h).cloned() else { continue };
        let Some(cmd) = command_for(&w, Os::current()).map(str::to_owned) else { continue };
        match w.mode {
            HookMode::Auto => {
                if run_one(e, &cmd)?.status != 0 && !e.state.pending_hooks.contains(&h) {
                    e.state.pending_hooks.push(h);
                    changed = true;
                }
            }
            HookMode::Ask => {
                if !e.state.pending_hooks.contains(&h) {
                    e.state.pending_hooks.push(h);
                    changed = true;
                    e.ctx.notifier.notify(Notification::HookPending { project: e.project_id, command: cmd });
                }
            }
            HookMode::Notify => e.ctx.notifier.notify(Notification::HookPending { project: e.project_id, command: cmd }),
        }
    }
    if changed {
        e.save()?;
    }
    Ok(())
}

/// Runs every pending hook in the project directory; successful ones leave the queue.
pub fn run_pending(e: &mut Engine) -> Result<Vec<HookRun>> {
    let mut runs = Vec::new();
    let mut still = Vec::new();
    for h in std::mem::take(&mut e.state.pending_hooks) {
        let Some(cmd) = e.config().watch.get(h).and_then(|w| command_for(w, Os::current())).map(str::to_owned) else { continue };
        let run = run_one(e, &cmd)?;
        if run.status != 0 {
            still.push(h);
        }
        runs.push(run);
    }
    e.state.pending_hooks = still;
    e.save()?;
    Ok(runs)
}

fn run_one(e: &Engine, cmd: &str) -> Result<HookRun> {
    let mut c = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };
    let (status, output) = match c.current_dir(&e.root).output() {
        Ok(o) => (o.status.code().unwrap_or(-1), [o.stdout, o.stderr].concat()),
        Err(err) => (-1, err.to_string().into_bytes()),
    };
    let start = output.len().saturating_sub(TAIL_BYTES);
    let output_tail = String::from_utf8_lossy(&output[start..]).into_owned();
    let mut log = std::fs::OpenOptions::new().create(true).append(true).open(e.data_dir().join("hooks.log"))?;
    writeln!(log, "[{}] $ {cmd}\n{output_tail}\n[exit {status}]", e.now())?;
    Ok(HookRun { command: cmd.to_owned(), status, output_tail })
}
