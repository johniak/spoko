//! The real `spoko` binary where the process boundary matters (stdin prompts, spawning the daemon).
#![cfg_attr(windows, allow(unused_imports, dead_code))] // helpers of #[cfg(unix)] tests

use std::path::PathBuf;
use std::process::{Command, Output};

use spoko_server::test_util::spawn_test_server;

struct Machine {
    dir: tempfile::TempDir,
}

impl Machine {
    fn new() -> Machine {
        let dir = tempfile::tempdir().unwrap();
        // every machine of the user has the account's SSH key
        let ssh = dir.path().join("h/.ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        std::fs::copy(spoko_crypto::test_util::key_path("id_ed25519"), ssh.join("id_ed25519")).unwrap();
        Machine { dir }
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
}

/// A child process that is killed if the test ends (or panics) while it still runs.
fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
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
    srv.create_account("jan", &spoko_crypto::test_util::key("id_ed25519"));
    let o = m.cmd(&["login", &srv.url]).env_remove("SPOKO_DEVICE_NAME").env_remove("HOSTNAME").env_remove("COMPUTERNAME").output().unwrap();
    assert!(o.status.success(), "{o:?}");
    assert!(stdout(&o).starts_with("Zalogowano jako komputer ("), "{}", stdout(&o));
}
