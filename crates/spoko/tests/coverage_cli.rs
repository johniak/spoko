//! The real `spoko` binary where the process boundary matters (stdin prompts, spawning the daemon).
#![cfg_attr(windows, allow(unused_imports, dead_code))] // helpers of #[cfg(unix)] tests

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use spoko_server::test_util::spawn_test_server;

struct Machine {
    dir: tempfile::TempDir,
}

impl Machine {
    fn new() -> Machine {
        Machine { dir: tempfile::tempdir().unwrap() }
    }
    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_spoko"));
        c.args(args)
            .current_dir(self.dir.path())
            .env("SPOKO_HOME", self.p("spoko-home"))
            .env("HOME", self.p("h"))
            .env("USERPROFILE", self.p("h"))
            .env("CLAUDE_CONFIG_DIR", self.p("claude"))
            .env("SPOKO_NO_DESKTOP_NOTIFY", "1");
        c
    }
    fn with_stdin(&self, args: &[&str], input: &str) -> Output {
        let mut child = self.cmd(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }
}

/// A child process that is killed if the test ends (or panics) while it still runs.
struct KillOnDrop(std::process::Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn device_add_waits_until_approval_answered_on_the_logged_in_machine() {
    let srv = spawn_test_server();
    let (mac, linux) = (Machine::new(), Machine::new());
    let o = mac.cmd(&["login", &srv.url, "--token", &srv.bootstrap_token("jan"), "--name", "mac"]).output().unwrap();
    assert!(o.status.success(), "{o:?}");
    let mut pairing = KillOnDrop(linux.cmd(&["device", "add", &srv.url, "--name", "linux"]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap());
    let mut out = BufReader::new(pairing.0.stdout.take().unwrap());
    let mut code = None;
    let mut line = String::new();
    while code.is_none() {
        line.clear();
        assert!(out.read_line(&mut line).unwrap() > 0, "pairing ended before printing a code");
        code = line.strip_prefix("Kod parowania: ").map(|c| c.trim().to_owned());
    }
    let code = code.unwrap();
    // answering anything but yes rejects; the new machine keeps waiting
    let no = mac.with_stdin(&["device", "approve", &code], "n\n");
    assert!(no.status.success());
    let text = stdout(&no);
    assert!(text.contains("Urządzenie „linux”, odcisk ") && text.contains("[t/N]"), "{text}");
    assert!(text.ends_with("Odrzucono.\n"), "{text}");
    assert!(pairing.0.try_wait().unwrap().is_none(), "still waiting for approval");
    let yes = mac.with_stdin(&["device", "approve", &code], "tak\n");
    assert!(stdout(&yes).ends_with("Zatwierdzono.\n"), "{yes:?}");
    assert!(pairing.0.wait().unwrap().success());
    let mut rest = String::new();
    out.read_to_string(&mut rest).unwrap();
    assert!(rest.contains("Sparowano: linux ("), "{rest}");
    let list = linux.cmd(&["device", "list"]).output().unwrap();
    assert!(stdout(&list).contains("mac\t") && stdout(&list).contains("linux (ten)\t"), "{list:?}");
}

#[test]
fn daemon_start_reports_a_daemon_that_never_comes_up() {
    let m = Machine::new();
    // not logged in: the spawned `spoko daemon run` exits right away
    let o = m.cmd(&["daemon", "start"]).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    let err = String::from_utf8_lossy(&o.stderr);
    assert_eq!(err, "błąd: daemon nie wystartował (spoko daemon run pokaże błąd)\n");
    assert!(stdout(&o).is_empty());
}

#[test]
fn unknown_subcommand_prints_usage_and_exits_2() {
    let o = Machine::new().cmd(&["frobnicate"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("unrecognized subcommand 'frobnicate'") && err.contains("Usage: spoko <COMMAND>"), "{err}");
}

#[test]
fn login_without_a_name_or_host_variables_calls_the_device_komputer() {
    let srv = spawn_test_server();
    let m = Machine::new();
    let o = m.cmd(&["login", &srv.url, "--token", &srv.bootstrap_token("jan")]).env_remove("SPOKO_DEVICE_NAME").env_remove("HOSTNAME").env_remove("COMPUTERNAME").output().unwrap();
    assert!(o.status.success(), "{o:?}");
    assert!(stdout(&o).starts_with("Zalogowano jako komputer ("), "{}", stdout(&o));
}
