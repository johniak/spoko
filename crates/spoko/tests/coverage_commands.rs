//! `spoko` commands run in this process (`commands::run`), with and without a daemon.
#![cfg_attr(windows, allow(unused_imports, dead_code))] // helpers of #[cfg(unix)] tests

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use spoko::cli::Cli;
use spoko::commands::{self, Ctx};
use spoko::docker_ext::DockerExt;
use spoko_client::daemon::ipc::{self, Request, Response};
use spoko_client::daemon::{DaemonOpts, Extensions};
use spoko_client::engine::Engine;
use spoko_client::notifier::NullNotifier;
use spoko_core::external::ExternalEnv;
use spoko_docker::api::DockerApi;
use spoko_docker::fake::FakeDocker;
use spoko_server::test_util::{TestServer, spawn_test_server};

/// One machine: its own SPOKO_HOME, home and Claude dir.
struct Machine {
    dir: tempfile::TempDir,
    /// What the user types when asked for the SSH key's passphrase.
    passphrase: fn(&str) -> std::io::Result<String>,
}

fn no_tty(_: &str) -> std::io::Result<String> {
    Err(std::io::Error::other("no terminal"))
}

fn types_sekret(prompt: &str) -> std::io::Result<String> {
    assert!(prompt.contains("id_ed25519"), "{prompt}");
    Ok("sekret".into())
}

fn types_wrong(_: &str) -> std::io::Result<String> {
    Ok("nie".into())
}

impl Machine {
    fn new() -> Machine {
        let dir = tempfile::tempdir().unwrap();
        // every machine of the user has the account's SSH key
        let ssh = dir.path().join("h/.ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        std::fs::copy(spoko_crypto::test_util::key_path("id_ed25519"), ssh.join("id_ed25519")).unwrap();
        Machine { dir, passphrase: no_tty }
    }

    /// A machine whose `~/.ssh/id_ed25519` has the passphrase `sekret`.
    fn with_passphrase_key() -> Machine {
        let m = Machine::new();
        std::fs::copy(spoko_crypto::test_util::key_path("id_ed25519_pass"), m.path("h/.ssh/id_ed25519")).unwrap();
        Machine { passphrase: types_sekret, ..m }
    }
    fn home(&self) -> PathBuf {
        self.dir.path().join("spoko-home")
    }
    fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }
    fn env(&self) -> ExternalEnv {
        ExternalEnv { home: self.path("h"), claude_config_dir: Some(self.path("claude")) }
    }
    fn run(&self, args: &[&str], cwd: &Path) -> Result<String, String> {
        let cli = Cli::try_parse_from(std::iter::once("spoko").chain(args.iter().copied())).unwrap();
        let mut ctx = Ctx { home: self.home(), env: self.env(), cwd: cwd.to_path_buf(), passphrase: self.passphrase };
        let mut out = Vec::new();
        let r = commands::run(&mut ctx, cli.cmd, &mut out);
        let out = String::from_utf8(out).unwrap();
        r.map(|()| out).map_err(|e| e.to_string())
    }
    fn ok(&self, args: &[&str], cwd: &Path) -> String {
        self.run(args, cwd).unwrap_or_else(|e| panic!("spoko {args:?} failed: {e}"))
    }
    fn fail(&self, args: &[&str], cwd: &Path) -> String {
        match self.run(args, cwd) {
            Ok(out) => panic!("spoko {args:?} should fail, printed:\n{out}"),
            Err(e) => e,
        }
    }
    fn top(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }
    /// A directory with `files`, canonicalized (as `init` stores it).
    fn dir_with(&self, rel: &str, files: &[(&str, &str)]) -> PathBuf {
        let p = self.path(rel);
        std::fs::create_dir_all(&p).unwrap();
        for (name, body) in files {
            std::fs::create_dir_all(p.join(name).parent().unwrap()).unwrap();
            std::fs::write(p.join(name), body).unwrap();
        }
        p.canonicalize().unwrap()
    }
}

fn recovery_key(out: &str) -> String {
    out.lines().find_map(|l| l.strip_prefix("Klucz odzyskiwania: ")).expect("recovery key printed").trim().to_owned()
}

fn project_id(out: &str) -> String {
    out.lines().find_map(|l| l.strip_prefix("Projekt: ")).expect("project id printed").trim().to_owned()
}

fn login(srv: &TestServer, m: &Machine, name: &str, rk: Option<&str>) -> String {
    srv.create_account("jan", &spoko_crypto::test_util::key("id_ed25519"));
    let mut args = vec!["login", &srv.url, "--name", name];
    if let Some(k) = rk {
        args.extend(["--recovery-key", k]);
    }
    m.ok(&args, &m.top())
}

/// Two machines of one account.
fn pair_of_machines(srv: &TestServer) -> (Machine, Machine) {
    let (a, b) = (Machine::new(), Machine::new());
    let rk = recovery_key(&login(srv, &a, "mac", None));
    login(srv, &b, "linux", Some(&rk));
    (a, b)
}

/// A daemon running in this process for `m`'s home; stopped on drop.
struct Daemon {
    home: PathBuf,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn start_daemon(m: &Machine, ext: Arc<dyn Extensions>) -> Daemon {
    let home = m.home();
    let opts = DaemonOpts { ext, notifier: Arc::new(NullNotifier), env: m.env(), rescan_every: Duration::from_secs(300) };
    let h = home.clone();
    let thread = std::thread::spawn(move || spoko_client::daemon::run(&h, opts).unwrap());
    let end = Instant::now() + Duration::from_secs(15);
    while ipc::request(&home, &Request::Status).is_err() {
        assert!(Instant::now() < end, "daemon did not come up");
        std::thread::sleep(Duration::from_millis(50));
    }
    Daemon { home, thread: Some(thread) }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = ipc::request(&self.home, &Request::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

/// Makes `root` a git repository whose own config picks the merge tool for `spoko resolve`.
fn merge_tool(root: &Path, tool: &str, cmd: Option<&str>) {
    let repo = git2::Repository::open(root).or_else(|_| git2::Repository::init(root)).unwrap();
    let mut cfg = repo.config().unwrap().open_level(git2::ConfigLevel::Local).unwrap();
    cfg.set_str("merge.tool", tool).unwrap();
    if let Some(c) = cmd {
        cfg.set_str(&format!("mergetool.{tool}.cmd"), c).unwrap();
    }
}

#[test]
fn unparsable_arguments_exit_with_clap_usage_code() {
    let mut out = Vec::new();
    assert_eq!(spoko::cli::main_with(["spoko", "frobnicate"], &mut out), 2);
    assert_eq!(spoko::cli::main_with(["spoko", "--version"], &mut out), 0);
    assert!(out.is_empty(), "clap prints usage itself, not into the command output");
}

#[test]
fn init_takes_a_path_relative_to_cwd_and_refuses_an_existing_project() {
    let srv = spawn_test_server();
    let m = Machine::new();
    login(&srv, &m, "mac", None);
    let out = m.ok(&["init", "sub/app"], &m.top());
    let id = project_id(&out);
    assert!(out.contains(&format!("Na innym komputerze: spoko clone {id} <katalog>")), "{out}");
    let root = m.path("sub/app").canonicalize().unwrap();
    let local = spoko_core::config::LocalConfig::load(&m.home()).unwrap();
    assert_eq!(local.projects.values().map(|p| p.path.clone()).collect::<Vec<_>>(), vec![root.clone()]);
    let err = m.fail(&["init", "app"], &m.path("sub"));
    assert_eq!(err, format!("{} już jest projektem spoko", root.display()));
}

#[test]
fn clone_into_a_non_empty_directory_keeps_local_differences_for_restore() {
    let srv = spawn_test_server();
    let (a, b) = pair_of_machines(&srv);
    let pa = a.dir_with("app", &[("notes.txt", "from mac\n"), ("same.txt", "same\n")]);
    let id = project_id(&a.ok(&["init"], &pa));
    let pb = b.dir_with("app", &[("notes.txt", "my local notes\n"), ("same.txt", "same\n")]);
    let out = b.ok(&["clone", &id, ".", "--join"], &pb);
    assert!(out.contains("Lokalne różnice (1 plików) zachowano w snapshocie dołączenia"), "{out}");
    assert_eq!(read(&pb.join("notes.txt")), "from mac\n");
    assert_eq!(b.ok(&["restore", "notes.txt", "--from-join"], &pb), "Przywrócono notes.txt\n");
    assert_eq!(read(&pb.join("notes.txt")), "my local notes\n");
    assert_eq!(b.fail(&["restore", "notes.txt"], &pb), "podaj --at <commit|czas> albo --from-join");
    // newest snapshot at or before a time in the future = the checked-out version
    let later = (spoko_client::engine::unix_now() + 3600).to_string();
    assert_eq!(b.ok(&["restore", "notes.txt", "--at", &later], &pb), "Przywrócono notes.txt\n");
    assert_eq!(read(&pb.join("notes.txt")), "from mac\n");
    assert!(b.fail(&["restore", "notes.txt", "--at", "1"], &pb).contains("no local snapshot at or before 1"));
}

#[test]
fn edits_to_different_files_on_two_machines_merge_without_conflicts() {
    let srv = spawn_test_server();
    let (a, b) = pair_of_machines(&srv);
    let pa = a.dir_with("app", &[("a.txt", "a\n"), ("b.txt", "b\n")]);
    let id = project_id(&a.ok(&["init"], &pa));
    std::fs::write(pa.join("c.txt"), "c\n").unwrap();
    a.ok(&["sync"], &pa);
    let pb = b.path("app");
    b.ok(&["clone", &id, pb.to_str().unwrap()], &b.top());
    let pb = pb.canonicalize().unwrap();
    std::fs::write(pa.join("a.txt"), "a from mac\n").unwrap();
    std::fs::write(pb.join("b.txt"), "b from linux\n").unwrap();
    assert_eq!(a.ok(&["sync"], &pa), format!("{}: wysłano\n", pa.display()));
    assert_eq!(b.ok(&["sync"], &pb), format!("{}: scalono\n", pb.display()));
    assert_eq!(read(&pb.join("a.txt")), "a from mac\n");
    // log of one file lists only the snapshots that changed it
    let all = a.ok(&["log"], &pa);
    let one = a.ok(&["log", "a.txt", "-n", "50"], &pa);
    assert!(all.lines().count() > one.lines().count(), "{all}\n--\n{one}");
    for line in one.lines() {
        let cols: Vec<&str> = line.split("  ").collect();
        assert_eq!(cols.len(), 3, "{line}");
        assert!(cols[0].len() == 12 && cols[0].chars().all(|c| c.is_ascii_hexdigit()), "{line}");
        assert!(cols[1].ends_with(" UTC"), "{line}");
        assert_eq!(cols[2], "mac");
    }
    let outside = a.path("elsewhere.txt");
    let outside = outside.to_str().unwrap();
    assert_eq!(a.fail(&["log", outside], &pa), format!("{outside} jest poza projektem"));
}

#[test]
fn conflicts_show_in_status_and_resolve_with_markers_or_the_merge_tool() {
    let srv = spawn_test_server();
    let (a, b) = pair_of_machines(&srv);
    let pa = a.dir_with("app", &[("f.txt", "base\n"), ("g.txt", "base\n")]);
    let id = project_id(&a.ok(&["init"], &pa));
    let pb = b.path("app");
    b.ok(&["clone", &id, pb.to_str().unwrap()], &b.top());
    let pb = pb.canonicalize().unwrap();
    for f in ["f.txt", "g.txt"] {
        std::fs::write(pa.join(f), "mac\n").unwrap();
        std::fs::write(pb.join(f), "linux\n").unwrap();
    }
    a.ok(&["sync"], &pa);
    assert!(b.ok(&["sync"], &pb).contains("konflikty: 2 (spoko resolve)"));
    let st = b.ok(&["status"], &pb);
    assert!(st.contains(&format!("app  {}  konflikty: 2 (spoko resolve)\n", pb.display())), "{st}");
    // a merge tool git does not know a command for: conflict markers
    merge_tool(&pb, "spoko-test-none", None);
    assert_eq!(b.ok(&["resolve", "g.txt"], &pb), "Plik zawiera wersje do scalenia; po edycji: spoko resolve g.txt --done\n");
    let g = read(&pb.join("g.txt"));
    assert!(g.contains("<<<<<<< local") && g.contains(">>>>>>> mac"), "{g}");
    assert_eq!(b.ok(&["resolve", "g.txt", "--done"], &pb), "Oznaczono jako rozwiązany: g.txt\n");
    let list = b.ok(&["resolve"], &pb);
    assert!(list.starts_with("f.txt  (druga wersja z mac, "), "{list}");
    #[cfg(unix)]
    {
        merge_tool(&pb, "spoko-test-take-theirs", Some("cp \"$REMOTE\" \"$MERGED\""));
        assert_eq!(b.ok(&["resolve", "f.txt"], &pb), "Rozwiązano: f.txt\n");
        assert_eq!(read(&pb.join("f.txt")), "mac\n");
        assert_eq!(b.ok(&["resolve"], &pb), "Brak konfliktów.\n");
    }
}

#[test]
fn offline_status_explains_why_sync_is_paused() {
    let srv = spawn_test_server();
    let m = Machine::new();
    login(&srv, &m, "mac", None);
    let many: Vec<(String, String)> = (0..100).map(|i| (format!("f{i:03}"), format!("{i}"))).collect();
    let refs: Vec<(&str, &str)> = many.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let big = m.dir_with("big", &refs);
    m.ok(&["init"], &big);
    for i in 0..60 {
        std::fs::remove_file(big.join(format!("f{i:03}"))).unwrap();
    }
    let gone = m.dir_with("gone", &[("a", "a")]);
    m.ok(&["init"], &gone);
    std::fs::remove_dir_all(&gone).unwrap();
    // outside any project: every project is synced
    let out = m.ok(&["sync"], &m.top());
    assert!(out.contains(&format!("{}: wstrzymany (spoko status)", big.display())), "{out}");
    assert!(out.contains(&format!("{}: wstrzymany (spoko status)", gone.display())), "{out}");
    let st = m.ok(&["status"], &m.top());
    assert!(st.starts_with("urządzenie: mac  daemon: nie działa\n"), "{st}");
    assert!(st.contains("big  ") && st.contains("wstrzymany: mass delete 60/100 (spoko resume)"), "{st}");
    assert!(st.contains("wstrzymany: project directory missing (spoko resume)"), "{st}");
}

#[test]
fn offline_status_shows_changes_waiting_when_the_server_is_unreachable() {
    let srv = spawn_test_server();
    let m = Machine::new();
    login(&srv, &m, "mac", None);
    let p = m.dir_with("app", &[("a.txt", "a")]);
    m.ok(&["init"], &p);
    assert!(m.ok(&["status"], &p).contains(&format!("app  {}  zsynchronizowany\n", p.display())));
    let _stopped = srv.stop();
    std::fs::write(p.join("a.txt"), "changed while offline").unwrap();
    m.fail(&["sync"], &p);
    let st = m.ok(&["status"], &p);
    assert!(st.contains("synchronizuje / zmiany czekają"), "{st}");
}

#[test]
fn pending_hooks_are_listed_run_and_dismissed() {
    let srv = spawn_test_server();
    let (a, b) = pair_of_machines(&srv);
    let config = "[[watch]]\nfiles = [\"package-lock.json\"]\nrun = \"echo deps-installed\"\n\n[[watch]]\nfiles = [\"Gemfile.lock\"]\nrun = \"exit 3\"\n";
    let pa = a.dir_with("app", &[(".spokoconfig", config), ("package-lock.json", "1"), ("Gemfile.lock", "1")]);
    let id = project_id(&a.ok(&["init"], &pa));
    let pb = b.path("app");
    b.ok(&["clone", &id, pb.to_str().unwrap()], &b.top());
    let pb = pb.canonicalize().unwrap();
    b.ok(&["hooks", "dismiss"], &pb);
    std::fs::write(pa.join("package-lock.json"), "2").unwrap();
    std::fs::write(pa.join("Gemfile.lock"), "2").unwrap();
    a.ok(&["sync"], &pa);
    b.ok(&["sync"], &pb);
    let st = b.ok(&["status"], &pb);
    assert!(st.contains("  czeka hook: echo deps-installed (spoko hooks run)\n") && st.contains("  czeka hook: exit 3 (spoko hooks run)\n"), "{st}");
    let runs = b.ok(&["hooks", "run"], &pb);
    assert!(runs.contains("$ echo deps-installed  → kod 0\ndeps-installed\n"), "{runs}");
    assert!(runs.contains("$ exit 3  → kod 3\n"), "{runs}");
    let st = b.ok(&["status"], &pb);
    assert!(!st.contains("echo deps-installed") && st.contains("czeka hook: exit 3"), "a failed hook stays pending:\n{st}");
    assert_eq!(b.ok(&["hooks", "dismiss"], &pb), "Pominięto oczekujące hooki.\n");
    assert!(!b.ok(&["status"], &pb).contains("czeka hook"));
    assert_eq!(b.ok(&["hooks", "run"], &pb), "Brak oczekujących hooków.\n");
}

#[test]
fn device_remove_logs_out_another_machine_only() {
    let srv = spawn_test_server();
    let (a, b) = pair_of_machines(&srv);
    assert_eq!(a.fail(&["device", "remove", "mac"], &a.top()), "to jest ten komputer");
    assert_eq!(a.fail(&["device", "remove", "phone"], &a.top()), "nie ma aktywnego urządzenia „phone”");
    let list = a.ok(&["device", "list"], &a.top());
    assert!(list.contains("mac (ten)\t") && list.contains("linux\t"), "{list}");
    assert!(list.lines().all(|l| l.ends_with("\taktywny")), "{list}");
    assert_eq!(a.ok(&["device", "remove", "linux"], &a.top()), "Wylogowano: linux\n");
    assert!(a.ok(&["device", "list"], &a.top()).contains("linux\t") && a.ok(&["device", "list"], &a.top()).contains("\todebrany"));
    assert!(b.fail(&["device", "list"], &b.top()).contains("unauthorized"));
    // the machine still has the key, so it can simply log in again
    login(&srv, &b, "linux", None);
    assert!(b.ok(&["device", "list"], &b.top()).contains("linux (ten)\t"));
}

#[test]
fn config_check_skips_git_descends_into_folders_and_counts_ignored_files() {
    let m = Machine::new();
    let p = m.dir_with("plain", &[(".gitignore", "*.log\n"), ("debug.log", "12345"), ("README", "hello")]);
    std::fs::create_dir_all(p.join(".git/objects")).unwrap();
    std::fs::write(p.join(".git/objects/blob"), "x".repeat(9000)).unwrap();
    std::fs::create_dir_all(p.join("src/deep")).unwrap();
    std::fs::write(p.join("src/deep/lib.rs"), "pub fn f() {}\n").unwrap();
    let out = m.ok(&["config", "check"], &p);
    assert!(out.starts_with(&format!("Katalog: {}\n", p.display())), "{out}");
    let row = |label: &str| out.lines().find(|l| l.starts_with(label)).map(|l| l.split_whitespace().skip(label.split_whitespace().count()).collect::<Vec<_>>().join(" ")).unwrap();
    assert_eq!(row("Synchronizowane"), "3 25 B");
    assert_eq!(row("Ignorowane"), "1 5 B");
    assert!(!out.contains("Ignorowane katalogi"), "no folder was skipped:\n{out}");
    assert!(out.contains("  src/deep/lib.rs\n"), "{out}");
    assert!(!out.contains(".git/"), "the 9000-byte .git object is neither synced nor listed:\n{out}");
}

#[test]
fn volume_commands_without_configured_volumes_fail_without_docker() {
    let srv = spawn_test_server();
    let m = Machine::new();
    login(&srv, &m, "mac", None);
    let p = m.dir_with("app", &[("a", "a")]);
    m.ok(&["init"], &p);
    let want = "brak takich wolumenów w .spokoconfig ([[docker.volume]])";
    assert_eq!(m.fail(&["volume", "push"], &p), want);
    assert_eq!(m.fail(&["volume", "pull", "pgdata", "--fresh", "--force"], &p), want);
}

fn fake_ext(docker: &Arc<FakeDocker>) -> Arc<dyn Extensions> {
    let d = docker.clone();
    Arc::new(DockerExt::with(Arc::new(move || Ok(d.clone() as Arc<dyn DockerApi>))))
}

#[test]
fn project_commands_go_through_a_running_daemon() {
    let srv = spawn_test_server();
    let m = Machine::new();
    login(&srv, &m, "mac", None);
    let mut files: Vec<(String, String)> = (0..100).map(|i| (format!("f{i:03}"), format!("{i}"))).collect();
    files.push((".spokoconfig".into(), "[[docker.volume]]\nname = \"pgdata\"\n".into()));
    files.push(("notes.txt".into(), "v1\n".into()));
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let p = m.dir_with("app", &refs);
    m.ok(&["init"], &p);
    // paused by a mass delete before the daemon starts
    for i in 0..60 {
        std::fs::remove_file(p.join(format!("f{i:03}"))).unwrap();
    }
    assert!(m.ok(&["sync"], &p).contains("wstrzymany"));
    let docker = Arc::new(FakeDocker::with_volume("app", "pgdata", b"rows", "postgres:16", "arm64"));
    let daemon = start_daemon(&m, fake_ext(&docker));
    assert_eq!(m.ok(&["daemon", "start"], &p), "daemon już działa\n");
    assert!(m.ok(&["status"], &p).starts_with("urządzenie: mac  daemon: działa\n"));

    let err = m.fail(&["resume"], &p);
    assert!(err.contains("confirm") && err.ends_with(" — uruchom: spoko resume --confirm"), "{err}");
    assert_eq!(m.ok(&["resume", "--confirm"], &p), format!("Wznowiono synchronizację {}\n", p.display()));
    assert_eq!(m.ok(&["sync"], &p), format!("{}: ok\n", p.display()));
    wait_for("synced after resume", || m.ok(&["status"], &p).contains("zsynchronizowany"));

    let text = m.ok(&["volume", "push"], &p);
    assert!(text.starts_with("pgdata: wysłano (") && text.ends_with(" nowych fragmentów)\n"), "{text}");
    assert_eq!(docker.take_log(), vec!["stop app-db", "export app_pgdata", "start app-db"]);
    assert_eq!(m.fail(&["volume", "pull", "redis"], &p), "brak takich wolumenów w .spokoconfig ([[docker.volume]])");

    let log: Vec<serde_json::Value> = serde_json::from_str(&m.ok(&["log", "notes.txt", "--json"], &p)).unwrap();
    let first = log.last().unwrap()["commit"].as_str().unwrap().to_owned();
    assert!(log.iter().all(|l| l["changed"] == true));
    assert!(m.ok(&["log"], &p).lines().any(|l| first.starts_with(l.split("  ").next().unwrap())));
    std::fs::write(p.join("notes.txt"), "v2\n").unwrap();
    assert_eq!(m.ok(&["restore", "notes.txt", "--at", &first], &p), "Przywrócono notes.txt\n");
    assert_eq!(read(&p.join("notes.txt")), "v1\n");

    assert_eq!(m.ok(&["resolve", "notes.txt", "--done"], &p), "ok\n");
    assert_eq!(m.ok(&["hooks", "run"], &p), "Brak oczekujących hooków.\n");
    assert_eq!(m.ok(&["hooks", "dismiss"], &p), "Pominięto oczekujące hooki.\n");
    assert_eq!(m.ok(&["pause"], &p), format!("Wstrzymano synchronizację {}\n", p.display()));
    assert!(m.ok(&["status"], &p).contains("wstrzymany: paused (spoko resume)"));
    m.ok(&["resume"], &p);

    // a broken .spokoconfig: the daemon keeps the changes locally and says why
    std::fs::write(p.join(".spokoconfig"), "bogus = 1\n").unwrap();
    wait_for("warning in status", || m.ok(&["status"], &p).contains("  uwaga: "));
    assert!(m.ok(&["status"], &p).contains(&format!("app  {}  offline (zmiany zapisane lokalnie)\n", p.display())));
    std::fs::write(p.join(".spokoconfig"), "").unwrap();

    assert_eq!(m.ok(&["daemon", "stop"], &p), "daemon zatrzymany\n");
    assert!(m.ok(&["status"], &p).starts_with("urządzenie: mac  daemon: nie działa\n"));
    assert_eq!(m.ok(&["daemon", "stop"], &p), "daemon zatrzymany\n", "stopping a stopped daemon is fine");
    drop(daemon);
}

/// A daemon extension that acknowledges volume requests without any text.
struct SilentVolumes;

impl Extensions for SilentVolumes {
    fn handle(&self, _e: &mut Engine, req: &Request) -> Option<Response> {
        matches!(req, Request::VolumePush { .. } | Request::VolumePull { .. }).then_some(Response::Ok)
    }
}

#[test]
fn volume_command_prints_ok_when_the_daemon_answers_without_text() {
    let srv = spawn_test_server();
    let m = Machine::new();
    login(&srv, &m, "mac", None);
    let p = m.dir_with("app", &[("a", "a")]);
    m.ok(&["init"], &p);
    let _daemon = start_daemon(&m, Arc::new(SilentVolumes));
    assert_eq!(m.ok(&["volume", "pull", "--force"], &p), "ok\n");
}

#[test]
fn bad_project_ids_are_rejected() {
    let srv = spawn_test_server();
    let m = Machine::new();
    login(&srv, &m, "mac", None);
    assert_eq!(m.fail(&["clone", "not-a-uuid", "x"], &m.top()), "„not-a-uuid” to nie jest identyfikator projektu");
    assert!(!m.path("x").exists(), "nothing created for a bad id");
}

#[test]
fn commands_on_a_logged_out_machine_ask_to_log_in() {
    let m = Machine::new();
    let p = m.dir_with("app", &[("a", "a")]);
    let mut local = spoko_core::config::LocalConfig::default();
    local.projects.insert(uuid::Uuid::new_v4(), spoko_core::config::LocalProject { path: p.clone(), paused: false });
    std::fs::create_dir_all(m.home()).unwrap();
    local.save(&m.home()).unwrap();
    let want = "ten komputer nie jest zalogowany (spoko login <serwer>)";
    assert_eq!(m.fail(&["hooks", "dismiss"], &p), want);
    assert_eq!(m.fail(&["status"], &p), want);
}

#[test]
fn restore_from_a_deleted_subdirectory_brings_the_file_back_there() {
    let srv = spawn_test_server();
    let m = Machine::new();
    login(&srv, &m, "mac", None);
    let p = m.dir_with("app", &[("top.txt", "t")]);
    std::fs::create_dir_all(p.join("docs")).unwrap();
    std::fs::write(p.join("docs/guide.md"), "# guide\n").unwrap();
    m.ok(&["init"], &p);
    let log: Vec<serde_json::Value> = serde_json::from_str(&m.ok(&["log", "--json"], &p)).unwrap();
    let commit = log[0]["commit"].as_str().unwrap().to_owned();
    // the shell still sits in docs/ after `rm -r docs`
    std::fs::remove_dir_all(p.join("docs")).unwrap();
    let cwd = p.join("docs");
    assert_eq!(m.ok(&["restore", "guide.md", "--at", &commit], &cwd), "Przywrócono docs/guide.md\n");
    assert_eq!(read(&p.join("docs/guide.md")), "# guide\n");
    // a directory that never existed outside any project
    let nowhere = m.path("nowhere");
    assert_eq!(m.fail(&["log"], &nowhere), format!("{} nie jest w żadnym projekcie spoko (spoko init / spoko clone)", nowhere.display()));
}

fn login_pass(srv: &TestServer, m: &Machine) -> String {
    let key = spoko_crypto::SshKey::load(&spoko_crypto::test_util::key_path("id_ed25519_pass"), Some("sekret")).unwrap();
    srv.create_account("p", &key);
    m.ok(&["login", &srv.url, "--name", "mac"], &m.top())
}

#[test]
fn login_with_a_passphrase_key_asks_for_it() {
    let srv = spawn_test_server();
    let m = Machine::with_passphrase_key();
    assert!(login_pass(&srv, &m).contains("Zalogowano jako mac"));
    // commands without the daemon ask again
    let p = m.dir_with("app", &[("a", "a")]);
    assert!(m.ok(&["init"], &p).contains("Projekt: "));
    let wrong = Machine { passphrase: types_wrong, ..m };
    assert_eq!(wrong.fail(&["status"], &wrong.top()), "crypto: wrong passphrase for the SSH key");
    let no_tty = Machine { passphrase: no_tty, ..wrong };
    assert_eq!(no_tty.fail(&["sync"], &p), "no terminal");
}

#[test]
fn unlock_prompts_and_unlocks_the_daemon() {
    let srv = spawn_test_server();
    let m = Machine::with_passphrase_key();
    login_pass(&srv, &m);
    let p = m.dir_with("app", &[("a", "a")]);
    m.ok(&["init"], &p);
    assert_eq!(m.fail(&["unlock"], &m.top()), "daemon nie działa (spoko daemon start)");
    let _daemon = start_daemon(&m, Arc::new(SilentVolumes));
    let st = m.ok(&["status"], &m.top());
    assert!(st.contains("daemon: zablokowany (spoko unlock)"), "{st}");
    let wrong = Machine { passphrase: types_wrong, ..m };
    assert_eq!(wrong.fail(&["unlock"], &wrong.top()), "crypto: wrong passphrase for the SSH key");
    let m = Machine { passphrase: types_sekret, ..wrong };
    assert_eq!(m.ok(&["unlock"], &m.top()), "Odblokowano.\n");
    assert_eq!(m.ok(&["unlock"], &m.top()), "daemon jest już odblokowany\n");
    let end = Instant::now() + Duration::from_secs(10);
    while !m.ok(&["status"], &m.top()).contains("zsynchronizowany") {
        assert!(Instant::now() < end, "project did not sync after unlock");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn key_rotate_moves_the_account_and_prints_a_new_recovery_key() {
    let srv = spawn_test_server();
    let (a, b) = pair_of_machines(&srv);
    let p = a.dir_with("app", &[("a", "a")]);
    a.ok(&["init"], &p);
    let new = spoko_crypto::test_util::key_path("id_ed25519_pass");
    let a = Machine { passphrase: types_sekret, ..a };
    let out = a.ok(&["key", "rotate", "--new", new.to_str().unwrap()], &a.top());
    let fp = spoko_crypto::SshKey::load(&new, Some("sekret")).unwrap().fingerprint();
    assert!(out.contains(&fp) && out.contains("Nowy klucz odzyskiwania: ") && out.contains("spoko login"), "{out}");
    // this machine keeps working with the new key, the other one has to log in again
    std::fs::write(p.join("b"), "b").unwrap();
    assert!(a.ok(&["sync"], &p).contains("wysłano"));
    assert!(b.fail(&["device", "list"], &b.top()).contains("unauthorized"));
    std::fs::copy(&new, b.path("h/.ssh/id_ed25519")).unwrap();
    let b = Machine { passphrase: types_sekret, ..b };
    b.ok(&["login", &srv.url, "--name", "linux"], &b.top());
    assert!(b.ok(&["device", "list"], &b.top()).contains("linux (ten)"));
    assert!(a.fail(&["key", "rotate", "--new", "/no/such/key"], &a.top()).starts_with("cannot read the SSH key: "));
}

#[test]
fn clone_without_a_path_makes_a_project_directory_and_touches_nothing_else() {
    let srv = spawn_test_server();
    let (a, b) = pair_of_machines(&srv);
    let pa = a.dir_with("app", &[("main.rs", "fn main() {}\n")]);
    let id = project_id(&a.ok(&["init"], &pa));
    // a parent folder full of other work, like ~/Projects
    let parent = b.dir_with("Projects", &[("other-repo/wip.rs", "uncommitted\n"), ("README.txt", "mine\n")]);
    let out = b.ok(&["clone", &id], &parent);
    assert!(out.contains(&format!("Katalog: {}", parent.join("app").display())), "{out}");
    assert_eq!(read(&parent.join("app/main.rs")), "fn main() {}\n");
    assert_eq!(read(&parent.join("other-repo/wip.rs")), "uncommitted\n");
    assert_eq!(read(&parent.join("README.txt")), "mine\n");
}

#[test]
fn clone_into_a_non_empty_directory_needs_join() {
    let srv = spawn_test_server();
    let (a, b) = pair_of_machines(&srv);
    let pa = a.dir_with("app", &[("main.rs", "fn main() {}\n")]);
    let id = project_id(&a.ok(&["init"], &pa));
    let other = b.dir_with("Projects", &[("other-repo/wip.rs", "uncommitted\n")]);
    let err = b.fail(&["clone", &id, "."], &other);
    assert!(err.contains("nie jest pusty") && err.contains("--join"), "{err}");
    assert_eq!(read(&other.join("other-repo/wip.rs")), "uncommitted\n");
    assert!(!other.join("main.rs").exists());
    assert!(spoko_core::config::LocalConfig::load(&b.home()).unwrap().projects.is_empty(), "nothing registered");
    // a target named like the project that already holds other things is refused too
    let parent = b.dir_with("P2", &[("app/mine.txt", "x")]);
    assert!(b.fail(&["clone", &id], &parent).contains("--join"));
    assert!(parent.join("app/mine.txt").exists());
}
