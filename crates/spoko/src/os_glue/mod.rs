//! Real OS / Docker connections (excluded from coverage; exercised by the docker CI job).

use std::sync::Arc;

use spoko_docker::api::DockerApi;
use spoko_docker::bollard_api::BollardApi;

use crate::docker_ext::DockerExt;

/// Volume sync against the local Docker daemon (socket / named pipe).
pub fn docker_ext() -> DockerExt {
    DockerExt::with(Arc::new(|| Ok(Arc::new(BollardApi::connect()?) as Arc<dyn DockerApi>)))
}

/// `spoko daemon install|uninstall`: changes the real login items (launchd / systemd / registry).
pub fn autostart(install: bool, out: &mut dyn std::io::Write) -> crate::commands::Res<()> {
    use spoko_client::os_glue::autostart;
    if install {
        autostart::install()?;
        writeln!(out, "daemon będzie startował po zalogowaniu")?;
    } else {
        autostart::uninstall()?;
        writeln!(out, "usunięto autostart daemona")?;
    }
    Ok(())
}

/// Starts `spoko daemon run` in the background, detached from this process's stdio.
pub fn spawn_daemon() -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args(["daemon", "run"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // without a console window when started from the tray
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
        no_inherit_stdio();
    }
    cmd.spawn().map(drop)
}

/// Windows children inherit every inheritable handle, including this process's stdout pipe:
/// a caller reading our output to the end would then wait for the daemon to exit.
#[cfg(windows)]
fn no_inherit_stdio() {
    use std::os::windows::io::AsRawHandle;
    unsafe extern "system" {
        fn SetHandleInformation(handle: *mut core::ffi::c_void, mask: u32, flags: u32) -> i32;
    }
    const HANDLE_FLAG_INHERIT: u32 = 1;
    for h in [std::io::stdin().as_raw_handle(), std::io::stdout().as_raw_handle(), std::io::stderr().as_raw_handle()] {
        if !h.is_null() {
            // SAFETY: a handle of this process's own standard streams; only its flags change
            unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0) };
        }
    }
}
