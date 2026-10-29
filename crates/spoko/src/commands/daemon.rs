use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use spoko_client::account::Identity;
use spoko_client::daemon::ipc::{self, Request, Response};
use spoko_client::daemon::{self, DaemonOpts};
use spoko_client::notifier::Notifier;
use spoko_client::os_glue::notify_native::NativeNotifier;

use super::{Ctx, PrintNotifier, Res, err};
use crate::cli::DaemonCmd;

fn wait(ctx: &Ctx, running: bool) -> Res<()> {
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        if ctx.daemon_running() == running {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    err(if running { "daemon nie wystartował (spoko daemon run pokaże błąd)" } else { "daemon nie zatrzymał się" })
}

pub fn run(ctx: &Ctx, cmd: Option<DaemonCmd>, out: &mut dyn Write) -> Res<()> {
    match cmd.unwrap_or(DaemonCmd::Run) {
        DaemonCmd::Run => {
            let notifier: Arc<dyn Notifier> = if std::env::var_os("SPOKO_NO_DESKTOP_NOTIFY").is_some() { Arc::new(PrintNotifier) } else { Arc::new(NativeNotifier) };
            let opts = DaemonOpts { ext: Arc::new(crate::os_glue::docker_ext()), notifier, env: ctx.env.clone(), rescan_every: Duration::from_secs(300) };
            crate::os_glue::spawn_tray();
            daemon::run(&ctx.home, opts)?;
        }
        DaemonCmd::Start => {
            if ctx.daemon_running() {
                writeln!(out, "daemon już działa")?;
                return Ok(());
            }
            crate::os_glue::spawn_daemon()?;
            wait(ctx, true)?;
            writeln!(out, "daemon uruchomiony")?;
            match ipc::request(&ctx.home, &Request::Status)? {
                Response::Status { report } if report.locked => writeln!(out, "klucz SSH ma hasło: odblokuj daemon poleceniem spoko unlock")?,
                Response::Status { report: ipc::StatusReport { problem: Some(p), .. } } => writeln!(out, "daemon czeka: {p}")?,
                _ => {}
            }
        }
        DaemonCmd::Stop => {
            if ctx.daemon_running() {
                ipc::request(&ctx.home, &Request::Shutdown)?;
                wait(ctx, false)?;
            }
            writeln!(out, "daemon zatrzymany")?;
        }
        DaemonCmd::Install => crate::os_glue::autostart(true, out)?,
        DaemonCmd::Uninstall => crate::os_glue::autostart(false, out)?,
    }
    Ok(())
}

fn locked(ctx: &Ctx) -> Res<bool> {
    Ok(matches!(ipc::request(&ctx.home, &Request::Status)?, Response::Status { report } if report.locked))
}

/// `spoko unlock`: the passphrase goes to the running daemon, which keeps the key in memory only.
pub fn unlock(ctx: &Ctx, out: &mut dyn Write) -> Res<()> {
    if !ctx.daemon_running() {
        return err("daemon nie działa (spoko daemon start)");
    }
    if !locked(ctx)? {
        writeln!(out, "daemon jest już odblokowany")?;
        return Ok(());
    }
    let passphrase = ctx.ask_passphrase(&Identity::key_path(&ctx.home)?)?;
    match ipc::request(&ctx.home, &Request::Unlock { passphrase })? {
        Response::Error { message } => err(message),
        _ => {
            writeln!(out, "Odblokowano.")?;
            Ok(())
        }
    }
}

/// After a new login or key the running daemon still has the old session: start it again.
pub fn restart_if_running(ctx: &Ctx, out: &mut dyn Write) -> Res<()> {
    if !ctx.daemon_running() {
        return Ok(());
    }
    run(ctx, Some(DaemonCmd::Stop), &mut std::io::sink())?;
    run(ctx, Some(DaemonCmd::Start), out)
}
