//! Start-at-login definitions (installed by `os_glue::autostart`).

pub const LAUNCHD_LABEL: &str = "dev.spoko.daemon";

pub fn launchd_plist(exe: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>daemon</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>
"#,
        exe = xml_escape(exe)
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn systemd_unit(exe: &str) -> String {
    format!("[Unit]\nDescription=spoko sync daemon\nAfter=network-online.target\n\n[Service]\nExecStart={exe} daemon\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n", exe = exe.replace(' ', "\\x20"))
}

/// Value for `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\spoko`.
pub fn windows_run_value(exe: &str) -> String {
    format!("\"{exe}\" daemon")
}
