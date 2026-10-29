use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use spoko_client::daemon::ipc::{self, Request};
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
