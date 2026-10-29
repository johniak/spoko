//! `cargo test --release -p spoko --test perf -- --ignored --nocapture` (results: docs/notes/perf.md)

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use spoko_server::test_util::spawn_test_server;

const FILES: usize = 100_000;

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
    fn cmd(&self, args: &[&str], cwd: &Path) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_spoko"));
        c.args(args)
            .current_dir(cwd)
            .env("SPOKO_HOME", self.p("spoko-home"))
            .env("HOME", self.p("h"))
            .env("CLAUDE_CONFIG_DIR", self.p("claude"))
            .env("SPOKO_NO_DESKTOP_NOTIFY", "1")
            .env("SPOKO_DEVICE_NAME", self.name);
        c
    }
    fn ok(&self, args: &[&str], cwd: &Path) -> String {
        let o = self.cmd(args, cwd).output().unwrap();
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    }
    fn daemon(&self) -> Child {
        let tmp = self.p("");
        let c = self.cmd(&["daemon", "run"], &tmp).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let end = Instant::now() + Duration::from_secs(60);
        while !String::from_utf8_lossy(&self.cmd(&["status"], &tmp).output().unwrap().stdout).contains("daemon: działa") {
            assert!(Instant::now() < end, "daemon did not start");
            std::thread::sleep(Duration::from_millis(100));
        }
        c
    }
}

fn ps(pid: u32, field: &str) -> String {
    let o = Command::new("ps").args(["-o", &format!("{field}="), "-p", &pid.to_string()]).output().unwrap();
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

/// `[[dd-]hh:]mm:ss[.ss]` → seconds
fn cpu_secs(t: &str) -> f64 {
    t.split(':').fold(0.0, |acc, part| acc * 60.0 + part.replace('-', "").parse::<f64>().unwrap_or(0.0))
}

fn pct(v: &mut [Duration], p: f64) -> Duration {
    v.sort();
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

fn two_machines(srv: &spoko_server::test_util::TestServer) -> (Machine, Machine) {
    let (a, b) = (Machine::new("mac"), Machine::new("linux"));
    let tmp = a.p("");
    srv.create_account("jan", &spoko_crypto::test_util::key("id_ed25519"));
    a.ok(&["login", &srv.url], &tmp);
    b.ok(&["login", &srv.url], &tmp);
    (a, b)
}

fn make_files(root: &Path, n: usize) {
    for i in 0..n {
        let p = root.join(format!("d{:03}/s{:02}/f{i}.txt", i % 500, i % 37));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, format!("file {i}\n")).unwrap();
    }
}

/// Spec: idle daemon < 50 MB RAM and ~0% CPU "for a typical project".
#[test]
#[ignore = "perf: release only"]
fn typical_project_idle_daemon() {
    const TYPICAL: usize = 5_000;
    let srv = spawn_test_server();
    let (a, _b) = two_machines(&srv);
    let pa = a.p("app");
    make_files(&pa, TYPICAL);
    a.ok(&["init"], &pa);
    let mut d = a.daemon();
    std::fs::write(pa.join("d000/s00/f0.txt"), "edit").unwrap();
    std::thread::sleep(Duration::from_secs(5));
    let rss_kb: u64 = ps(d.id(), "rss").parse().unwrap();
    let cpu0 = cpu_secs(&ps(d.id(), "time"));
    std::thread::sleep(Duration::from_secs(30));
    let idle_cpu = (cpu_secs(&ps(d.id(), "time")) - cpu0) / 30.0 * 100.0;
    let _ = d.kill();
    let _ = d.wait();
    let report = format!("typical project ({TYPICAL} files): idle daemon RSS {} MiB, CPU over 30 s {idle_cpu:.2}%\n", rss_kb / 1024);
    eprintln!("{report}");
    std::fs::write(std::env::temp_dir().join("spoko-perf-typical.txt"), &report).unwrap();
    assert!(rss_kb < 50 * 1024, "RSS {rss_kb} KiB");
    assert!(idle_cpu < 1.0, "idle CPU {idle_cpu}%");
}

#[test]
#[ignore = "perf: release only"]
fn scale_100k() {
    let srv = spawn_test_server();
    let (a, b) = two_machines(&srv);
    let tmp = a.p("");
    let pa = a.p("big");
    make_files(&pa, FILES);
    let t = Instant::now();
    let id = a.ok(&["init"], &pa).lines().find_map(|l| l.strip_prefix("Projekt: ").map(str::to_owned)).unwrap();
    let init = t.elapsed();
    let t = Instant::now();
    let pb = b.p("big");
    b.ok(&["clone", &id, pb.to_str().unwrap()], &tmp);
    let clone = t.elapsed();
    let mut rescans = vec![];
    for _ in 0..5 {
        let t = Instant::now();
        assert!(a.ok(&["sync"], &pa).contains("aktualny"));
        rescans.push(t.elapsed());
    }
    let full_scan = pct(&mut rescans, 0.5);

    let (mut da, mut db) = (a.daemon(), b.daemon());
    std::thread::sleep(Duration::from_secs(2));
    let mut lat = vec![];
    for i in 0..50 {
        let rel = format!("d{:03}/s{:02}/f{}.txt", i % 500, i % 37, i);
        let content = format!("changed {i}\n");
        let t = Instant::now();
        std::fs::write(pa.join(&rel), &content).unwrap();
        while std::fs::read_to_string(pb.join(&rel)).ok().as_deref() != Some(content.as_str()) {
            assert!(t.elapsed() < Duration::from_secs(30), "sample {i} timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
        lat.push(t.elapsed());
        std::thread::sleep(Duration::from_millis(200));
    }
    let (p50, p95) = (pct(&mut lat, 0.5), pct(&mut lat, 0.95));

    std::thread::sleep(Duration::from_secs(5));
    let rss_kb: u64 = ps(da.id(), "rss").parse().unwrap();
    let cpu0 = cpu_secs(&ps(da.id(), "time"));
    std::thread::sleep(Duration::from_secs(60));
    let cpu1 = cpu_secs(&ps(da.id(), "time"));
    let idle_cpu = (cpu1 - cpu0) / 60.0 * 100.0;

    let report = format!(
        "files: {FILES}\ninit (scan+hash+upload): {init:?}\nclone (download+checkout): {clone:?}\nfull rescan via `spoko sync`, no change (p50 of 5): {full_scan:?}\nsmall change A→B through daemons: p50 {p50:?}, p95 {p95:?} (50 samples)\nidle daemon RSS (informational at this scale): {} MiB\nidle daemon CPU over 60 s: {idle_cpu:.2}%\n",
        rss_kb / 1024
    );
    eprintln!("{report}");
    std::fs::write(std::env::temp_dir().join("spoko-perf.txt"), &report).unwrap();
    for c in [&mut da, &mut db] {
        let _ = c.kill();
        let _ = c.wait();
    }
    assert!(full_scan < Duration::from_secs(2), "full scan {full_scan:?}");
    assert!(p50 < Duration::from_secs(1) && p95 < Duration::from_secs(3), "latency p50 {p50:?} p95 {p95:?}");
    assert!(idle_cpu < 1.0, "idle CPU {idle_cpu}%");
}
