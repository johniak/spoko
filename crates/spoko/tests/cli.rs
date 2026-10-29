use std::path::{Path, PathBuf};

use assert_cmd::Command;
use spoko_server::test_util::{TestServer, spawn_test_server};

/// One machine: its own SPOKO_HOME, HOME and Claude dir (never the real ones).
struct Machine {
    dir: tempfile::TempDir,
}

impl Machine {
    /// The machine has the account's SSH key at `~/.ssh/id_ed25519` (like every machine of the user).
    fn new() -> Machine {
        let dir = tempfile::tempdir().unwrap();
        let ssh = dir.path().join("h/.ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        std::fs::copy(spoko_crypto::test_util::key_path("id_ed25519"), ssh.join("id_ed25519")).unwrap();
        Machine { dir }
    }
    fn home(&self) -> PathBuf {
        self.dir.path().join("spoko-home")
    }
    fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }
    fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("spoko").unwrap();
        c.env("SPOKO_NO_DESKTOP_NOTIFY", "1").env("SPOKO_HOME", self.home()).env("HOME", self.dir.path().join("h")).env("USERPROFILE", self.dir.path().join("h")).env("CLAUDE_CONFIG_DIR", self.dir.path().join("claude"));
        c
    }
    fn run(&self, args: &[&str], cwd: Option<&Path>) -> (bool, String, String) {
        let mut c = self.cmd();
        c.args(args);
        if let Some(d) = cwd {
            c.current_dir(d);
        }
        let o = c.output().unwrap();
        (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
    }
    fn ok(&self, args: &[&str], cwd: Option<&Path>) -> String {
        let (ok, out, err) = self.run(args, cwd);
        assert!(ok, "spoko {args:?} failed:\n{out}\n{err}");
        out
    }
}

fn recovery_key(out: &str) -> String {
    out.lines().find_map(|l| l.strip_prefix("Klucz odzyskiwania: ")).expect("recovery key printed").trim().to_owned()
}

fn login(srv: &TestServer, m: &Machine, name: &str, rk: Option<&str>) -> String {
    srv.create_account("jan", &spoko_crypto::test_util::key("id_ed25519"));
    let mut args = vec!["login", &srv.url, "--name", name];
    if let Some(k) = rk {
        args.extend(["--recovery-key", k]);
    }
    m.ok(&args, None)
}

fn project_id(out: &str) -> String {
    out.lines().find_map(|l| l.strip_prefix("Projekt: ")).expect("project id printed").trim().to_owned()
}

#[test]
fn first_login_prints_recovery_key() {
    let srv = spawn_test_server();
    let m = Machine::new();
    let out = login(&srv, &m, "mac", None);
    let rk = recovery_key(&out);
    assert!(rk.len() > 20);
    let list = m.ok(&["device", "list"], None);
    assert!(list.contains("mac"), "{list}");
    let json: serde_json::Value = serde_json::from_str(&m.ok(&["device", "list", "--json"], None)).unwrap();
    assert_eq!(json[0]["name"], "mac");
    // a second machine with the same SSH key joins the same account
    let m2 = Machine::new();
    let out2 = login(&srv, &m2, "linux", None);
    assert!(!out2.contains("Klucz odzyskiwania"));
    assert!(m2.ok(&["device", "list"], None).contains("linux"));
    // a key the server does not know
    let other = spoko_crypto::test_util::key_path("other_ed25519");
    let (ok, _, err) = m.run(&["login", &srv.url, "--key", other.to_str().unwrap(), "--name", "x"], None);
    assert!(!ok && err.contains("błąd"), "{err}");
    // no key at all
    std::fs::remove_file(m.path("h/.ssh/id_ed25519")).unwrap();
    let (ok, _, err) = m.run(&["login", &srv.url, "--name", "x"], None);
    assert!(!ok && err.contains("ssh-keygen -t ed25519"), "{err}");
}

#[test]
fn init_status_shows_synced() {
    let srv = spawn_test_server();
    let m = Machine::new();
    login(&srv, &m, "mac", None);
    let p = m.path("proj");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(p.join("a.txt"), "a").unwrap();
    let out = m.ok(&["init", p.to_str().unwrap()], None);
    project_id(&out);
    let st = m.ok(&["status"], None);
    assert!(st.contains("proj") && st.contains("zsynchronizowany"), "{st}");
    let (ok, _, err) = m.run(&["sync"], Some(&m.path("")));
    assert!(ok, "sync outside a project syncs all: {err}");
    let (ok, _, err) = m.run(&["log"], Some(&m.path("")));
    assert!(!ok && err.contains("nie jest w żadnym projekcie"), "{err}");
}

#[test]
fn two_homes_init_clone_edit_sync() {
    let srv = spawn_test_server();
    let (a, b) = (Machine::new(), Machine::new());
    let rk = recovery_key(&login(&srv, &a, "mac", None));
    login(&srv, &b, "linux", Some(&rk));
    let pa = a.path("work/app");
    std::fs::create_dir_all(&pa).unwrap();
    std::fs::write(pa.join("main.rs"), "fn main() {}\n").unwrap();
    let id = project_id(&a.ok(&["init"], Some(&pa)));
    let pb = b.path("code/app");
    b.ok(&["clone", &id, pb.to_str().unwrap()], None);
    assert_eq!(std::fs::read_to_string(pb.join("main.rs")).unwrap(), "fn main() {}\n");
    std::fs::write(pa.join("main.rs"), "fn main() { println!(\"a\"); }\n").unwrap();
    let out = a.ok(&["sync"], Some(&pa));
    assert!(out.contains("wysłano"), "{out}");
    let out = b.ok(&["sync"], Some(&pb.join(".")));
    assert!(out.contains("pobrano"), "{out}");
    assert_eq!(std::fs::read_to_string(pb.join("main.rs")).unwrap(), "fn main() { println!(\"a\"); }\n");
    // history and restore
    let log: serde_json::Value = serde_json::from_str(&a.ok(&["log", "main.rs", "--json"], Some(&pa))).unwrap();
    let first = log.as_array().unwrap().last().unwrap()["commit"].as_str().unwrap().to_owned();
    a.ok(&["restore", "main.rs", "--at", &first], Some(&pa));
    assert_eq!(std::fs::read_to_string(pa.join("main.rs")).unwrap(), "fn main() {}\n");
    assert!(a.ok(&["resolve"], Some(&pa)).contains("Brak konfliktów"));
    assert!(a.ok(&["hooks", "run"], Some(&pa)).contains("Brak"));
    let (ok, _, _) = b.run(&["clone", "00000000-0000-0000-0000-000000000000", b.path("x").to_str().unwrap()], None);
    assert!(!ok);
}

#[test]
fn config_check_lists_classes() {
    let srv = spawn_test_server();
    let m = Machine::new();
    login(&srv, &m, "mac", None);
    let p = m.path("proj");
    std::fs::create_dir_all(p.join("node_modules/react")).unwrap();
    std::fs::write(p.join(".gitignore"), "node_modules/\n").unwrap();
    std::fs::write(p.join(".spokoconfig"), "[[watch]]\nfiles = [\"package-lock.json\"]\nrun = \"npm ci\"\n").unwrap();
    std::fs::write(p.join("node_modules/react/index.js"), "x").unwrap();
    std::fs::write(p.join("package-lock.json"), "{}").unwrap();
    std::fs::write(p.join("logo.png"), b"\x89PNG\0\0\0binary").unwrap();
    std::fs::write(p.join("big.txt"), "x".repeat(5000)).unwrap();
    let out = m.ok(&["config", "check"], Some(&p));
    for needle in ["Synchronizowane", "Ignorowane", "Binarne", "Watch", "big.txt", "node_modules"] {
        assert!(out.contains(needle), "missing {needle}:\n{out}");
    }
    std::fs::write(p.join(".spokoconfig"), "bogus = 1\n").unwrap();
    let (ok, _, err) = m.run(&["config", "check"], Some(&p));
    assert!(!ok && err.contains("błąd"), "{err}");
}

#[test]
fn resume_requires_confirm_after_mass_delete() {
    let srv = spawn_test_server();
    let m = Machine::new();
    login(&srv, &m, "mac", None);
    let p = m.path("proj");
    std::fs::create_dir_all(&p).unwrap();
    for i in 0..100 {
        std::fs::write(p.join(format!("f{i:03}")), format!("{i}")).unwrap();
    }
    m.ok(&["init"], Some(&p));
    for i in 0..60 {
        std::fs::remove_file(p.join(format!("f{i:03}"))).unwrap();
    }
    let out = m.ok(&["sync"], Some(&p));
    assert!(out.contains("wstrzymany"), "{out}");
    let (ok, _, err) = m.run(&["resume"], Some(&p));
    assert!(!ok && err.contains("--confirm"), "{err}");
    m.ok(&["resume", "--confirm"], Some(&p));
    assert!(m.ok(&["sync"], Some(&p)).contains("wysłano"));
    m.ok(&["pause"], Some(&p));
    assert!(m.ok(&["status"], None).contains("wstrzymany"));
    m.ok(&["resume"], Some(&p));
}

#[test]
fn status_json_schema() {
    let srv = spawn_test_server();
    let m = Machine::new();
    login(&srv, &m, "mac", None);
    let p = m.path("proj");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(p.join("a"), "a").unwrap();
    let id = project_id(&m.ok(&["init", "--name", "My App"], Some(&p)));
    let v: serde_json::Value = serde_json::from_str(&m.ok(&["status", "--json"], None)).unwrap();
    assert_eq!(v["device_name"], "mac");
    let pr = &v["projects"][0];
    assert_eq!(pr["id"], id);
    assert_eq!(pr["state"]["state"], "synced");
    assert_eq!(pr["conflicts"], 0);
    assert!(pr["root"].as_str().unwrap().ends_with("proj"));
    assert!(pr["pending_hooks"].as_array().unwrap().is_empty());
}

#[test]
fn daemon_start_status_stop() {
    let srv = spawn_test_server();
    let m = Machine::new();
    login(&srv, &m, "mac", None);
    let p = m.path("proj");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(p.join("a"), "a").unwrap();
    m.ok(&["init"], Some(&p));
    m.ok(&["daemon", "start"], None);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let (ok, out, _) = m.run(&["status"], None);
        if ok && out.contains("daemon: działa") {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "daemon did not come up: {out}");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // project commands go through the daemon now
    assert!(m.ok(&["sync"], Some(&p)).contains("ok"));
    // a project added while the daemon runs is synced by it straight away
    let q = m.path("second");
    std::fs::create_dir_all(&q).unwrap();
    std::fs::write(q.join("b"), "b").unwrap();
    m.ok(&["init"], Some(&q));
    assert!(m.ok(&["sync"], Some(&q)).contains("ok"));
    assert!(m.ok(&["status"], None).contains("second"));
    m.ok(&["daemon", "stop"], None);
    assert!(m.ok(&["status"], None).contains("daemon: nie działa"));
}

#[test]
fn passphrase_key_daemon_starts_locked_and_unlocks_from_stdin() {
    let srv = spawn_test_server();
    let m = Machine::new();
    std::fs::copy(spoko_crypto::test_util::key_path("id_ed25519_pass"), m.path("h/.ssh/id_ed25519")).unwrap();
    srv.create_account("p", &spoko_crypto::SshKey::load(&spoko_crypto::test_util::key_path("id_ed25519_pass"), Some("sekret")).unwrap());
    let with_stdin = |args: &[&str], input: &str| {
        let o = m.cmd().args(args).write_stdin(input).output().unwrap();
        (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
    };
    let (ok, out, err) = with_stdin(&["login", &srv.url, "--name", "mac"], "sekret\n");
    assert!(ok && out.contains("Zalogowano jako mac"), "{out}{err}");
    let out = m.ok(&["daemon", "start"], None);
    assert!(out.contains("spoko unlock"), "{out}");
    assert!(m.ok(&["status"], None).contains("daemon: zablokowany"));
    let (ok, _, err) = with_stdin(&["unlock"], "zle\n");
    assert!(!ok && err.contains("wrong passphrase"), "{err}");
    let (ok, out, err) = with_stdin(&["unlock"], "sekret\n");
    assert!(ok && out == "Odblokowano.\n", "{out}{err}");
    assert!(m.ok(&["status"], None).contains("daemon: działa"));
    m.ok(&["daemon", "stop"], None);
}

#[test]
fn daemon_started_before_login_waits_and_says_so() {
    let m = Machine::new();
    let out = m.ok(&["daemon", "start"], None);
    assert!(out.contains("daemon uruchomiony") && out.contains("spoko login"), "{out}");
    let st = m.ok(&["status"], None);
    assert!(st.contains("daemon: czeka") && st.contains("spoko login"), "{st}");
    m.ok(&["daemon", "stop"], None);
}

/// `code --wait` and friends need the user's terminal and environment, not the daemon's.
#[cfg(unix)]
#[test]
fn merge_tool_runs_in_the_cli_process_while_the_daemon_runs() {
    let srv = spawn_test_server();
    let (a, b) = (Machine::new(), Machine::new());
    let rk = recovery_key(&login(&srv, &a, "mac", None));
    login(&srv, &b, "linux", Some(&rk));
    let pa = a.path("app");
    std::fs::create_dir_all(&pa).unwrap();
    std::fs::write(pa.join("f.txt"), "base\n").unwrap();
    let id = project_id(&a.ok(&["init"], Some(&pa)));
    let pb = b.path("app");
    b.ok(&["clone", &id, pb.to_str().unwrap()], None);
    std::fs::write(pa.join("f.txt"), "mac\n").unwrap();
    std::fs::write(pb.join("f.txt"), "linux\n").unwrap();
    a.ok(&["sync"], Some(&pa));
    assert!(b.ok(&["sync"], Some(&pb)).contains("konflikty: 1"));
    let repo = git2::Repository::init(&pb).unwrap();
    let mut cfg = repo.config().unwrap().open_level(git2::ConfigLevel::Local).unwrap();
    cfg.set_str("merge.tool", "mine").unwrap();
    cfg.set_str("mergetool.mine.cmd", "printf '%s\\n' \"$ONLY_IN_MY_SHELL\" > \"$MERGED\"").unwrap();
    b.ok(&["daemon", "start"], None);
    let o = b.cmd().args(["resolve", "f.txt"]).current_dir(&pb).env("ONLY_IN_MY_SHELL", "from the terminal").output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(o.status.success() && out == "Rozwiązano: f.txt\n", "{out}{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(std::fs::read_to_string(pb.join("f.txt")).unwrap(), "from the terminal\n");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !b.ok(&["resolve"], Some(&pb)).contains("Brak konfliktów") {
        assert!(std::time::Instant::now() < deadline, "the daemon marked it resolved");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    b.ok(&["daemon", "stop"], None);
}
