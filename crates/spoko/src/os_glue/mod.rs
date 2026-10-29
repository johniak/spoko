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
#[cfg(not(windows))]
pub fn spawn_daemon() -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    Command::new(std::env::current_exe()?).args(["daemon", "run"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(drop)
}

/// Windows children inherit every inheritable handle, including this process's stdout pipe,
/// so a caller reading our output to the end would wait for the daemon to exit. Clearing the
/// flag needs `unsafe` FFI; `Start-Process` (ShellExecute) starts it without inheriting any.
#[cfg(windows)]
pub fn spawn_daemon() -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let exe = std::env::current_exe()?.to_string_lossy().replace('\'', "''");
    let script = format!("Start-Process -FilePath '{exe}' -ArgumentList 'daemon','run' -WindowStyle Hidden");
    let st = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()?;
    if st.success() { Ok(()) } else { Err(std::io::Error::other(format!("Start-Process failed: {st}"))) }
}

/// The SSH key's passphrase: typed without echo in a terminal, one line from a pipe otherwise.
pub fn read_passphrase(prompt: &str) -> std::io::Result<String> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return rpassword::prompt_password(prompt);
    }
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}
