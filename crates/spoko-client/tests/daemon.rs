mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use spoko_client::daemon::ipc::{self, Request, Response, SyncState};
use spoko_client::daemon::watcher::{Debouncer, MAX_DELAY, QUIET};
use spoko_client::daemon::{self, DaemonOpts, NoExtensions, autostart};
use spoko_client::engine::Engine;
use spoko_core::config::{LocalConfig, LocalProject};
use spoko_core::platform::RelPath;
use support::*;
use uuid::Uuid;

#[test]
fn debounce_coalesces_burst() {
    let root = PathBuf::from("/proj");
    let mut d = Debouncer::new(std::slice::from_ref(&root));
    let t0 = Instant::now();
    for i in 0..100 {
        d.push(&root.join(format!("f{i}")), t0 + Duration::from_micros(500 * i));
    }
    d.push(Path::new("/elsewhere/x"), t0);
    d.push(&root.join(".spoko-tmp-abc"), t0);
    assert!(d.flush_due(t0 + Duration::from_millis(50)).is_empty());
    assert!(d.next_deadline().is_some());
    let out = d.flush_due(t0 + Duration::from_millis(50) + QUIET);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].paths.len(), 100, "temp files of our own writes are ignored");
    assert_eq!(out[0].root, root);
    assert!(d.next_deadline().is_none());
}

#[test]
fn debounce_max_latency() {
    let root = PathBuf::from("/proj");
    let mut d = Debouncer::new(std::slice::from_ref(&root));
    let t0 = Instant::now();
    let mut flushed_at = vec![];
    for i in 0..200u64 {
        let now = t0 + Duration::from_millis(10 * i);
        d.push(&root.join("hot.log"), now);
        if !d.flush_due(now).is_empty() {
            flushed_at.push(now);
        }
    }
    assert!(flushed_at.len() >= 3, "{flushed_at:?}");
    let mut prev = t0;
    for t in flushed_at {
        assert!(t.duration_since(prev) <= MAX_DELAY + Duration::from_millis(10));
        prev = t;
    }
}

#[test]
fn git_paths_flag_git_dirty() {
    let root = PathBuf::from("/proj");
    let mut d = Debouncer::new(std::slice::from_ref(&root));
    let t0 = Instant::now();
    d.push(&root.join(".git/index"), t0);
    assert!(d.flush_due(t0 + QUIET).is_empty(), "git-only batches settle longer");
    let out = d.flush_due(t0 + Duration::from_secs(1));
    assert!(out[0].git_dirty && out[0].paths.is_empty());
    d.push(&root.join(".git/HEAD"), t0);
    d.push(&root.join("src/a.rs"), t0);
    let out = d.flush_due(t0 + QUIET);
    assert!(out[0].git_dirty);
    assert_eq!(out[0].paths.iter().map(RelPath::as_str).collect::<Vec<_>>(), vec!["src/a.rs"]);
    d.push_rescan(t0);
    d.push(&root, t0);
    assert!(d.flush_due(t0 + QUIET)[0].rescan);
}

#[test]
fn autostart_templates() {
    let plist = autostart::launchd_plist("/Applications/spoko & co/spoko");
    assert!(plist.contains("<key>RunAtLoad</key>\n    <true/>"));
    assert!(plist.contains("<string>/Applications/spoko &amp; co/spoko</string>"));
    assert!(plist.contains("<string>daemon</string>"));
    let unit = autostart::systemd_unit("/usr/local/bin/spoko");
    assert!(unit.contains("ExecStart=/usr/local/bin/spoko daemon"));
    assert!(unit.contains("WantedBy=default.target"));
    assert_eq!(autostart::windows_run_value(r"C:\Program Files\spoko\spoko.exe"), r#""C:\Program Files\spoko\spoko.exe" daemon"#);
}

/// Registers the project in the device's LocalConfig and starts its daemon in a thread.
fn start_daemon(d: &Device, project: Uuid) -> std::thread::JoinHandle<spoko_client::Result<()>> {
    let mut lc = LocalConfig::load(&d.home).unwrap();
    lc.projects.insert(project, LocalProject { path: d.root.clone(), paused: false });
    lc.save(&d.home).unwrap();
    let home = d.home.clone();
    let opts = DaemonOpts { ext: Arc::new(NoExtensions), notifier: d.notes.clone(), env: d.ctx.env.clone(), rescan_every: Duration::from_secs(300) };
    let h = std::thread::spawn(move || daemon::run(&home, opts));
    wait_for(Duration::from_secs(10), || ipc::request(&d.home, &Request::Status).is_ok());
    h
}

fn stop_daemon(d: &Device, h: std::thread::JoinHandle<spoko_client::Result<()>>) {
    assert_eq!(ipc::request(&d.home, &Request::Shutdown).unwrap(), Response::Ok);
    h.join().unwrap().unwrap();
}

fn wait_for(timeout: Duration, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("condition not met within {timeout:?}");
}

#[test]
fn daemon_end_to_end_two_homes() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("README.md", "hi\n");
    let project = a.create().project_id;
    drop(b.join(project));
    let (ha, hb) = (start_daemon(&a, project), start_daemon(&b, project));
    std::thread::sleep(Duration::from_millis(300)); // watchers armed
    let t = Instant::now();
    a.write("src/new.rs", "fn new() {}\n");
    wait_for(Duration::from_secs(20), || b.read("src/new.rs").is_some_and(|c| c == b"fn new() {}\n"));
    eprintln!("A → B latency through watcher + server: {:?}", t.elapsed());
    // and back
    b.write("src/new.rs", "fn new() { /* b */ }\n");
    wait_for(Duration::from_secs(20), || a.read("src/new.rs").is_some_and(|c| c == b"fn new() { /* b */ }\n"));
    stop_daemon(&a, ha);
    stop_daemon(&b, hb);
}

#[test]
fn ipc_status_roundtrip() {
    let mut w = World::new();
    let a = w.device("a");
    a.write("x", "1");
    let project = a.create().project_id;
    let h = start_daemon(&a, project);
    wait_for(Duration::from_secs(10), || match ipc::request(&a.home, &Request::Status).unwrap() {
        Response::Status { report } => report.projects.len() == 1 && report.projects[0].state == SyncState::Synced && report.projects[0].root == a.root,
        _ => false,
    });
    let Response::Status { report } = ipc::request(&a.home, &Request::Status).unwrap() else { panic!() };
    assert_eq!(report.device_name, "a");
    assert!(report.projects[0].last_sync.is_some());
    assert_eq!(ipc::request(&a.home, &Request::Sync { project }).unwrap(), Response::Ok);
    assert_eq!(ipc::request(&a.home, &Request::Pause { project }).unwrap(), Response::Ok);
    let Response::Status { report } = ipc::request(&a.home, &Request::Status).unwrap() else { panic!() };
    assert!(matches!(report.projects[0].state, SyncState::Paused(_)));
    assert_eq!(ipc::request(&a.home, &Request::Resume { project, confirm: false }).unwrap(), Response::Ok);
    let Response::Log { entries } = ipc::request(&a.home, &Request::Log { project, path: None, limit: 10 }).unwrap() else { panic!() };
    assert_eq!(entries.len(), 1);
    assert_eq!(ipc::request(&a.home, &Request::HooksRun { project }).unwrap(), Response::Hooks { runs: vec![] });
    assert!(matches!(ipc::request(&a.home, &Request::VolumePush { project, volume: None }).unwrap(), Response::Error { .. }));
    assert!(matches!(ipc::request(&a.home, &Request::Sync { project: Uuid::nil() }).unwrap(), Response::Error { .. }));
    assert!(matches!(ipc::request(&a.home, &Request::Resolve { project, path: "x".into(), done: false }).unwrap(), Response::Error { .. }));
    let restore = Request::Restore { project, path: "x".into(), from: ipc::RestoreSpec::Commit { oid: entries[0].commit.clone() } };
    a.write("x", "changed");
    assert_eq!(ipc::request(&a.home, &restore).unwrap(), Response::Ok);
    assert_eq!(a.text("x"), "1");
    stop_daemon(&a, h);
    assert!(ipc::request(&a.home, &Request::Status).is_err(), "daemon gone");
}

#[test]
fn second_daemon_refuses_lock() {
    let mut w = World::new();
    let a = w.device("a");
    a.write("x", "1");
    let project = a.create().project_id;
    let h = start_daemon(&a, project);
    let opts = DaemonOpts { ext: Arc::new(NoExtensions), notifier: a.notes.clone(), env: a.ctx.env.clone(), rescan_every: Duration::from_secs(300) };
    let err = daemon::run(&a.home, opts).unwrap_err();
    assert!(err.to_string().contains("already running"), "{err}");
    stop_daemon(&a, h);
}

#[test]
fn offline_then_reconnect_syncs() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("f", "1");
    let project = a.create().project_id;
    drop(b.join(project));
    let (ha, hb) = (start_daemon(&a, project), start_daemon(&b, project));
    std::thread::sleep(Duration::from_millis(300));
    let srv = std::mem::replace(&mut w.srv, spoko_server::test_util::spawn_test_server());
    let stopped = srv.stop();
    a.write("f", "edited offline");
    a.write("g", "new offline");
    wait_for(Duration::from_secs(10), || match ipc::request(&a.home, &Request::Status).unwrap() {
        Response::Status { report } => report.projects[0].state == SyncState::Offline,
        _ => false,
    });
    w.srv = stopped.restart();
    wait_for(Duration::from_secs(30), || b.read("g").is_some() && b.read("f").is_some_and(|c| c == b"edited offline"));
    stop_daemon(&a, ha);
    stop_daemon(&b, hb);
}

#[test]
fn hooks_dismiss_goes_through_the_daemon() {
    let mut w = World::new();
    let a = w.device("a");
    a.write(".spokoconfig", "[[watch]]\nfiles = [\"package-lock.json\"]\nrun = \"npm ci\"\nmode = \"ask\"\n");
    let mut e = a.create();
    let project = e.project_id;
    e.state.pending_hooks = vec![0];
    e.save().unwrap();
    drop(e);
    let h = start_daemon(&a, project);
    let pending = || match ipc::request(&a.home, &Request::Status).unwrap() {
        Response::Status { report } => report.projects[0].pending_hooks.clone(),
        other => panic!("{other:?}"),
    };
    wait_for(Duration::from_secs(10), || pending() == vec!["npm ci".to_owned()]);
    assert_eq!(ipc::request(&a.home, &Request::HooksDismiss { project }).unwrap(), Response::Ok);
    assert!(pending().is_empty());
    stop_daemon(&a, h);
    assert!(a.open(project).state.pending_hooks.is_empty(), "the daemon's own state no longer has them");
}

#[test]
fn locked_daemon_reports_locked_then_syncs_after_unlock() {
    let mut w = World::with_key("id_ed25519_pass", Some("sekret"));
    let a = w.device("a");
    a.write("x", "1");
    let project = a.create().project_id;
    a.write("x", "2");
    let h = start_daemon(&a, project);
    let Response::Status { report } = ipc::request(&a.home, &Request::Status).unwrap() else { panic!() };
    assert!(report.locked && report.projects.is_empty(), "{report:?}");
    let Response::Error { message } = ipc::request(&a.home, &Request::Sync { project }).unwrap() else { panic!() };
    assert!(message.contains("spoko unlock"), "{message}");
    let wrong = ipc::request(&a.home, &Request::Unlock { passphrase: "nie".into() }).unwrap();
    assert_eq!(wrong, Response::Error { message: "crypto: wrong passphrase for the SSH key".into() });
    let Response::Status { report } = ipc::request(&a.home, &Request::Status).unwrap() else { panic!() };
    assert!(report.locked, "still locked");
    assert_eq!(ipc::request(&a.home, &Request::Unlock { passphrase: "sekret".into() }).unwrap(), Response::Ok);
    wait_for(Duration::from_secs(10), || match ipc::request(&a.home, &Request::Status).unwrap() {
        Response::Status { report } => !report.locked && report.projects.len() == 1 && report.projects[0].state == SyncState::Synced,
        _ => false,
    });
    // the change made while locked was synced
    let Response::Log { entries } = ipc::request(&a.home, &Request::Log { project, path: None, limit: 10 }).unwrap() else { panic!() };
    assert_eq!(entries.len(), 2);
    assert_eq!(ipc::request(&a.home, &Request::Unlock { passphrase: "sekret".into() }).unwrap(), Response::Ok, "already unlocked");
    stop_daemon(&a, h);
}

#[test]
fn locked_daemon_shuts_down_cleanly() {
    let mut w = World::with_key("id_ed25519_pass", Some("sekret"));
    let a = w.device("a");
    a.write("x", "1");
    let project = a.create().project_id;
    let h = start_daemon(&a, project);
    stop_daemon(&a, h);
    assert!(ipc::request(&a.home, &Request::Status).is_err(), "daemon gone");
}

#[cfg(unix)]
#[test]
fn ipc_socket_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let mut w = World::new();
    let a = w.device("a");
    a.write("x", "1");
    let project = a.create().project_id;
    let h = start_daemon(&a, project);
    let mode = std::fs::metadata(ipc::socket_path(&a.home)).unwrap().permissions().mode();
    assert_eq!(mode & 0o077, 0, "socket mode {mode:o}: a passphrase crosses it");
    stop_daemon(&a, h);
}

#[test]
fn reload_picks_up_a_project_added_while_running() {
    let mut w = World::with_key("id_ed25519_pass", Some("sekret"));
    let a = w.device("a");
    a.write("x", "1");
    let first = a.create().project_id;
    let h = start_daemon(&a, first);
    assert_eq!(ipc::request(&a.home, &Request::Unlock { passphrase: "sekret".into() }).unwrap(), Response::Ok);
    // a second project, registered the way `spoko init` does it
    let root2 = a.root.parent().unwrap().join("second");
    std::fs::create_dir_all(&root2).unwrap();
    std::fs::write(root2.join("y"), "2").unwrap();
    let second = Engine::create(a.ctx.clone(), &root2, "second").unwrap().project_id;
    let mut lc = LocalConfig::load(&a.home).unwrap();
    lc.projects.insert(second, LocalProject { path: root2.clone(), paused: false });
    lc.save(&a.home).unwrap();
    assert!(matches!(ipc::request(&a.home, &Request::Sync { project: second }).unwrap(), Response::Error { .. }), "not known yet");
    assert_eq!(ipc::request(&a.home, &Request::Reload).unwrap(), Response::Ok);
    wait_for(Duration::from_secs(10), || match ipc::request(&a.home, &Request::Status) {
        Ok(Response::Status { report }) => !report.locked && report.projects.len() == 2,
        _ => false,
    });
    assert_eq!(ipc::request(&a.home, &Request::Sync { project: second }).unwrap(), Response::Ok, "no new passphrase needed");
    // and the watcher covers it: a change there gets captured
    std::fs::write(root2.join("y"), "changed").unwrap();
    wait_for(Duration::from_secs(10), || match ipc::request(&a.home, &Request::Log { project: second, path: None, limit: 10 }) {
        Ok(Response::Log { entries }) => entries.len() >= 2,
        _ => false,
    });
    stop_daemon(&a, h);
}
