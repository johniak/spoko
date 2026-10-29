//! The spec's whole story with real `spoko` processes (CLI + daemons) on one real server.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use spoko_server::test_util::spawn_test_server;

struct Machine {
    dir: tempfile::TempDir,
    name: &'static str,
}

impl Machine {
    /// The machine has the account's SSH key at `~/.ssh/id_ed25519`, as every machine of the user.
    fn new(name: &'static str) -> Machine {
        let dir = tempfile::tempdir().unwrap();
        let ssh = dir.path().join("h/.ssh");
        std::fs::create_dir_all(&ssh).unwrap();
        std::fs::copy(spoko_crypto::test_util::key_path("id_ed25519"), ssh.join("id_ed25519")).unwrap();
        Machine { dir, name }
    }
    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }
    fn claude(&self) -> PathBuf {
        self.p("claude")
    }
    fn cmd(&self, args: &[&str], cwd: &Path) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_spoko"));
        c.args(args)
            .current_dir(cwd)
            .env("SPOKO_HOME", self.p("spoko-home"))
            .env("HOME", self.p("h"))
            .env("USERPROFILE", self.p("h"))
            .env("CLAUDE_CONFIG_DIR", self.claude())
            .env("SPOKO_NO_DESKTOP_NOTIFY", "1")
            .env("SPOKO_DEVICE_NAME", self.name);
        c
    }
    fn run(&self, args: &[&str], cwd: &Path) -> (bool, String) {
        let o = self.cmd(args, cwd).output().unwrap();
        (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
    }
    fn ok(&self, args: &[&str], cwd: &Path) -> String {
        let (ok, out) = self.run(args, cwd);
        assert!(ok, "[{}] spoko {args:?}:\n{out}", self.name);
        out
    }
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(30);
    let start = Instant::now();
    while Instant::now() < end {
        if f() {
            eprintln!("{what}: {:?}", start.elapsed());
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for: {what}");
}

fn read(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok()
}

fn git(cwd: &Path, args: &[&str]) {
    let st = Command::new("git").args(["-c", "user.name=Jan", "-c", "user.email=jan@example.com"]).args(args).current_dir(cwd).output().unwrap();
    assert!(st.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&st.stderr));
}

fn chat_dir(m: &Machine, root: &Path) -> PathBuf {
    m.claude().join("projects").join(spoko_core::external::encode_claude_dir(&root.to_string_lossy()))
}

fn json_line(root: &Path, uuid: &str) -> String {
    format!("{{\"uuid\":\"{uuid}\",\"cwd\":{}}}\n", serde_json::to_string(&root.to_string_lossy()).unwrap())
}

#[test]
fn whole_story() {
    let srv = spawn_test_server();
    let (a, b) = (Machine::new("mac"), Machine::new("linux"));
    let tmp = a.p("");

    // 1. the server admin creates the account for the user's SSH key; both machines log in with it
    srv.create_account("jan", &spoko_crypto::test_util::key("id_ed25519"));
    let out = a.ok(&["login", &srv.url], &tmp);
    assert!(out.contains("Klucz odzyskiwania") && out.contains("SHA256:"), "{out}");
    let out = b.ok(&["login", &srv.url], &tmp);
    assert!(!out.contains("Klucz odzyskiwania"), "one recovery key per account");
    assert!(b.ok(&["device", "list"], &tmp).contains("mac"));

    // 2. project on A: git repo, .env synced via include, node_modules not, a watch hook
    let pa = a.p("work/app");
    std::fs::create_dir_all(pa.join("node_modules/x")).unwrap();
    let pa = pa.canonicalize().unwrap();
    std::fs::write(pa.join(".gitignore"), ".env\nnode_modules/\n.hook-ran\n").unwrap();
    std::fs::write(pa.join(".spokoconfig"), "[ignore]\ninclude = [\".env\"]\n\n[[watch]]\nfiles = [\"package-lock.json\"]\nrun = \"echo installed > .hook-ran\"\nmode = \"auto\"\n").unwrap();
    std::fs::write(pa.join("package-lock.json"), "{\"v\":1}").unwrap();
    std::fs::write(pa.join("main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(pa.join("node_modules/x/i.js"), "big").unwrap();
    git(&pa, &["init", "-q", "-b", "main"]);
    git(&pa, &["add", "-A"]);
    git(&pa, &["commit", "-qm", "init"]);
    std::fs::write(pa.join(".env"), "DB=postgres://a\n").unwrap();
    let chats_a = chat_dir(&a, &pa);
    std::fs::create_dir_all(&chats_a).unwrap();
    std::fs::write(chats_a.join("s.jsonl"), json_line(&pa, "1")).unwrap();
    let id = a.ok(&["init"], &pa).lines().find_map(|l| l.strip_prefix("Projekt: ").map(str::to_owned)).unwrap();

    // 3. clone on B (different path)
    let pb = b.p("src/elsewhere/app");
    std::fs::create_dir_all(&pb).unwrap();
    let pb = pb.canonicalize().unwrap();
    std::fs::create_dir_all(chat_dir(&b, &pb)).unwrap(); // existing dir so the daemon watches it
    b.ok(&["clone", &id, pb.to_str().unwrap()], &tmp);
    assert_eq!(read(&pb.join(".env")).as_deref(), Some("DB=postgres://a\n"));
    assert!(!pb.join("node_modules").exists());
    assert_eq!(read(&chat_dir(&b, &pb).join("s.jsonl")).unwrap(), json_line(&pb, "1"));
    assert!(read(&pb.join(".hook-ran")).is_some_and(|s| s.contains("installed")), "auto hook ran on first checkout");

    // 4. daemons
    a.ok(&["daemon", "start"], &tmp);
    b.ok(&["daemon", "start"], &tmp);
    std::thread::sleep(Duration::from_millis(500));

    // 5. live edit A → B, and B → A
    std::fs::write(pa.join("main.rs"), "fn main() { println!(\"a\"); }\n").unwrap();
    wait_for("edit A→B", || read(&pb.join("main.rs")).is_some_and(|s| s.contains("println")));
    std::fs::write(pb.join("lib.rs"), "pub fn b() {}\n").unwrap();
    wait_for("new file B→A", || read(&pa.join("lib.rs")).is_some());

    // 6. git: commit on johniak, switch to dzban → B is on dzban with the commit
    git(&pa, &["checkout", "-qb", "johniak"]);
    git(&pa, &["add", "-A"]);
    git(&pa, &["commit", "-qm", "johniak work"]);
    git(&pa, &["checkout", "-qb", "dzban"]);
    wait_for("HEAD → dzban on B", || read(&pb.join(".git/HEAD")).is_some_and(|h| h.trim() == "ref: refs/heads/dzban"));
    let log = Command::new("git").args(["log", "--format=%s", "-1", "johniak"]).current_dir(&pb).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&log.stdout).trim(), "johniak work");
    let st = Command::new("git").args(["status", "--porcelain"]).current_dir(&pb).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&st.stdout).trim(), "", "B's git status is clean");

    // 7. binary file and Claude chat
    let png: Vec<u8> = (0..=255u8).cycle().take(300_000).collect();
    std::fs::write(pa.join("logo.png"), &png).unwrap();
    wait_for("binary A→B", || std::fs::read(pb.join("logo.png")).is_ok_and(|b| b == png));
    std::fs::write(chats_a.join("s.jsonl"), format!("{}{}", json_line(&pa, "1"), json_line(&pa, "2"))).unwrap();
    wait_for("chat A→B with B's path", || read(&chat_dir(&b, &pb).join("s.jsonl")).is_some_and(|s| s == format!("{}{}", json_line(&pb, "1"), json_line(&pb, "2"))));

    // 8. lockfile change runs the hook on B
    std::fs::remove_file(pb.join(".hook-ran")).unwrap();
    std::fs::write(pa.join("package-lock.json"), "{\"v\":2}").unwrap();
    wait_for("hook on B", || read(&pb.join(".hook-ran")).is_some());

    // 9. conflict while both are offline from each other (daemons stopped)
    a.ok(&["daemon", "stop"], &tmp);
    b.ok(&["daemon", "stop"], &tmp);
    std::fs::write(pa.join("main.rs"), "fn main() { /* A */ }\n").unwrap();
    std::fs::write(pb.join("main.rs"), "fn main() { /* B */ }\n").unwrap();
    a.ok(&["sync"], &pa);
    assert!(b.ok(&["sync"], &pb).contains("konflikty: 1"));
    assert!(b.ok(&["resolve"], &pb).contains("main.rs"));
    std::fs::write(pb.join("main.rs"), "fn main() { /* A + B */ }\n").unwrap();
    b.ok(&["resolve", "main.rs", "--done"], &pb);
    b.ok(&["sync"], &pb);
    a.ok(&["sync"], &pa);
    assert_eq!(read(&pa.join("main.rs")).unwrap(), "fn main() { /* A + B */ }\n");
    assert!(a.ok(&["resolve"], &pa).contains("Brak konfliktów"));

    // 10. A logs B out: B stops syncing until it logs in again with the key
    assert_eq!(a.ok(&["device", "remove", "linux"], &tmp), "Wylogowano: linux\n");
    let (ok, out) = b.run(&["sync"], &pb);
    assert!(!ok, "a logged-out device must not sync: {out}");
    std::fs::write(pa.join("after.txt"), "x").unwrap();
    assert!(a.ok(&["sync"], &pa).contains("wysłano"), "A keeps working");
    b.ok(&["login", &srv.url], &tmp);
    b.ok(&["sync"], &pb);
    assert_eq!(read(&pb.join("after.txt")).unwrap(), "x");

    // 11. new SSH key while A's daemon runs: the daemon restarts on the new key, B follows
    a.ok(&["daemon", "start"], &tmp);
    let new_key = a.p("h/.ssh/id_new");
    std::fs::copy(spoko_crypto::test_util::key_path("id_ecdsa"), &new_key).unwrap();
    let out = a.ok(&["key", "rotate", "--new", new_key.to_str().unwrap()], &tmp);
    assert!(out.contains("Nowy klucz odzyskiwania") && out.contains("daemon uruchomiony"), "{out}");
    assert!(a.ok(&["status"], &tmp).contains("daemon: działa"));
    std::fs::write(pa.join("rotated.txt"), "r").unwrap();
    // goes through the restarted daemon
    a.ok(&["sync"], &pa);
    let (ok, _) = b.run(&["sync"], &pb);
    assert!(!ok, "B still has the old session");
    b.ok(&["login", &srv.url, "--key", new_key.to_str().unwrap()], &tmp);
    b.ok(&["sync"], &pb);
    assert_eq!(read(&pb.join("rotated.txt")).unwrap(), "r");
    a.ok(&["daemon", "stop"], &tmp);
}
