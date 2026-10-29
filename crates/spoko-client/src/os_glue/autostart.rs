//! Installs/removes the start-at-login entry for this OS.

use std::process::Command;

use crate::daemon::autostart::{LAUNCHD_LABEL, launchd_plist, systemd_unit, windows_run_value};
use crate::{ClientError, Result};

fn exe() -> Result<String> {
    Ok(std::env::current_exe()?.to_string_lossy().into_owned())
}

fn home() -> Result<std::path::PathBuf> {
    spoko_core::config::home_dir().ok_or_else(|| ClientError::Other("no home directory".into()))
}

fn run(cmd: &mut Command) -> Result<()> {
    let st = cmd.status()?;
    if st.success() { Ok(()) } else { Err(ClientError::Other(format!("{cmd:?} failed: {st}"))) }
}

pub fn install() -> Result<()> {
    let exe = exe()?;
    if cfg!(target_os = "macos") {
        let p = home()?.join("Library/LaunchAgents").join(format!("{LAUNCHD_LABEL}.plist"));
        std::fs::create_dir_all(p.parent().expect("has parent"))?;
        std::fs::write(&p, launchd_plist(&exe))?;
        let _ = Command::new("launchctl").arg("unload").arg(&p).status();
        run(Command::new("launchctl").arg("load").arg(&p))
    } else if cfg!(windows) {
        run(Command::new("reg").args(["add", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", "spoko", "/t", "REG_SZ", "/f", "/d", &windows_run_value(&exe)]))
    } else {
        let p = home()?.join(".config/systemd/user/spoko.service");
        std::fs::create_dir_all(p.parent().expect("has parent"))?;
        std::fs::write(&p, systemd_unit(&exe))?;
        run(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
        run(Command::new("systemctl").args(["--user", "enable", "--now", "spoko.service"]))
    }
}

pub fn uninstall() -> Result<()> {
    if cfg!(target_os = "macos") {
        let p = home()?.join("Library/LaunchAgents").join(format!("{LAUNCHD_LABEL}.plist"));
        let _ = Command::new("launchctl").arg("unload").arg(&p).status();
        let _ = std::fs::remove_file(p);
        Ok(())
    } else if cfg!(windows) {
        run(Command::new("reg").args(["delete", r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run", "/v", "spoko", "/f"]))
    } else {
        let _ = Command::new("systemctl").args(["--user", "disable", "--now", "spoko.service"]).status();
        let _ = std::fs::remove_file(home()?.join(".config/systemd/user/spoko.service"));
        Ok(())
    }
}
