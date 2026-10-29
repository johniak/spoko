//! Daemon paths the end-to-end tests do not reach: the project worker driven directly, project
//! messages and volume prompts through a running daemon, the IPC server and the debounce loop.
#![cfg_attr(windows, allow(unused_imports, dead_code))] // helpers of #[cfg(unix)] tests

mod support;

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, bounded, unbounded};
use spoko_client::account::{self, Identity};
use spoko_client::daemon::ipc::{self, ProjectStatus, Request, Response, RestoreSpec, SyncState};
use spoko_client::daemon::project_worker::{Worker, WorkerMsg};
use spoko_client::daemon::watcher::debounce_loop;
use spoko_client::daemon::{self, DaemonOpts, Extensions, NoExtensions};
use spoko_client::engine::{Engine, SyncOutcome};
use spoko_client::notifier::Notification;
use spoko_core::config::{LocalConfig, LocalProject};
use support::*;
use uuid::Uuid;

const LONG: Duration = Duration::from_secs(3600);

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

struct Running {
    tx: Sender<WorkerMsg>,
    status: Arc<Mutex<ProjectStatus>>,
    thread: std::thread::JoinHandle<()>,
}

impl Running {
    fn start(engine: Engine, rescan_every: Duration) -> Running {
        let status = Arc::new(Mutex::new(ProjectStatus {
            id: engine.project_id,
            name: "proj".into(),
            root: engine.root.clone(),
            state: SyncState::Syncing,
            last_sync: None,
            pending_hooks: vec![],
            conflicts: 0,
            warnings: vec![],
            volume_prompts: vec![],
        }));
        let (tx, rx) = unbounded();
        let worker = Worker::new(engine, Arc::new(NoExtensions), status.clone(), rescan_every);
        let thread = std::thread::spawn(move || worker.run(&rx));
        let r = Running { tx, status, thread };
        r.wait_state(|s| s != &SyncState::Syncing);
        r
    }
    fn status(&self) -> ProjectStatus {
        self.status.lock().unwrap().clone()
    }
    fn wait_state(&self, f: impl Fn(&SyncState) -> bool) {
        wait_for(Duration::from_secs(20), || f(&self.status().state));
    }
    fn ask(&self, req: Request) -> Response {
        let (tx, rx) = bounded(1);
        self.tx.send(WorkerMsg::Ipc(req, tx)).unwrap();
        rx.recv_timeout(Duration::from_secs(20)).unwrap()
    }
    fn stop(self) {
        self.tx.send(WorkerMsg::Shutdown).unwrap();
        self.thread.join().unwrap();
    }
}

fn error_text(r: Response) -> String {
    match r {
        Response::Error { message } => message,
        other => panic!("expected an error, got {other:?}"),
    }
}

#[test]
fn worker_exits_when_its_channel_closes() {
    let mut w = World::new();
    let a = w.device("a");
    a.write("x", "1");
    let r = Running::start(a.create(), LONG);
    assert_eq!(r.status().state, SyncState::Synced);
    assert!(r.status().last_sync.is_some());
    drop(r.tx);
    wait_for(Duration::from_secs(5), || r.thread.is_finished());
    r.thread.join().unwrap();
}

#[test]
fn worker_rescans_periodically_without_file_events() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("x", "1");
    let ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    let r = Running::start(ea, Duration::from_millis(100));
    a.write("later.txt", "picked up by the rescan");
    wait_for(Duration::from_secs(20), || {
        eb.sync().unwrap();
        b.read("later.txt").is_some()
    });
    assert_eq!(b.text("later.txt"), "picked up by the rescan");
    r.stop();
}

#[test]
fn rescan_message_triggers_a_full_sync() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("x", "1");
    let ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    let r = Running::start(ea, LONG);
    a.write("asked.txt", "rescan");
    r.tx.send(WorkerMsg::Rescan).unwrap();
    wait_for(Duration::from_secs(20), || {
        eb.sync().unwrap();
        b.read("asked.txt").is_some()
    });
    r.stop();
}

#[test]
fn failed_sync_request_reports_the_error_and_status_stays_offline() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("x", "1");
    let ea = a.create();
    let project = ea.project_id;
    let r = Running::start(ea, LONG);
    let synced_at = r.status().last_sync;
    let idb = Identity::load(&b.home, None).unwrap();
    account::remove_device(&idb, a.ctx.device_id).unwrap();
    a.write("x", "2");
    let msg = error_text(r.ask(Request::Sync { project }));
    assert_eq!(msg, "unauthorized (device revoked or token invalid)");
    let st = r.status();
    assert_eq!(st.state, SyncState::Offline);
    assert_eq!(st.warnings, vec![msg.clone()]);
    assert_eq!(st.last_sync, synced_at, "a failed sync is not a sync");
    // other requests answered while offline do not make the project look synced
    let log = r.ask(Request::Log {
        project,
        path: None,
        limit: 5,
    });
    assert!(matches!(log, Response::Log { .. }), "{log:?}");
    let st = r.status();
    assert_eq!(
        (st.state, st.warnings, st.last_sync),
        (SyncState::Offline, vec![msg], synced_at)
    );
    r.stop();
}

#[test]
fn mass_delete_pauses_the_worker() {
    let mut w = World::new();
    let a = w.device("a");
    for i in 0..60 {
        a.write(&format!("f{i}"), "x");
    }
    let r = Running::start(a.create(), LONG);
    for i in 0..60 {
        a.rm(&format!("f{i}"));
    }
    r.tx.send(WorkerMsg::Rescan).unwrap();
    r.wait_state(|s| matches!(s, SyncState::Paused(_)));
    assert_eq!(
        r.status().state,
        SyncState::Paused("mass delete 60/60".into())
    );
    r.stop();
}

#[test]
fn missing_root_pauses_the_worker() {
    let mut w = World::new();
    let a = w.device("a");
    a.write("x", "1");
    let r = Running::start(a.create(), LONG);
    std::fs::remove_dir_all(&a.root).unwrap();
    r.tx.send(WorkerMsg::Rescan).unwrap();
    r.wait_state(|s| matches!(s, SyncState::Paused(_)));
    assert_eq!(
        r.status().state,
        SyncState::Paused("project directory missing".into())
    );
    r.stop();
}

#[test]
fn conflict_is_resolved_through_worker_requests() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("f.txt", "base\n");
    let mut ea = a.create();
    let project = ea.project_id;
    let (mut eb, _) = b.join(project);
    a.write("f.txt", "from a\n");
    b.write("f.txt", "from b\n");
    ea.sync().unwrap();
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Merged { conflicts: 1 });
    // a merge tool without a command: conflict markers, whatever the user's global git config
    let repo = init_repo(&b.root);
    repo.config()
        .unwrap()
        .set_str("merge.tool", "spoko-test-none")
        .unwrap();
    let r = Running::start(eb, LONG);
    assert_eq!(r.status().state, SyncState::Conflict(1));
    let path = "f.txt".to_owned();
    let out = r.ask(Request::Resolve {
        project,
        path: path.clone(),
        done: false,
    });
    assert_eq!(
        out,
        Response::Text {
            text: "unchanged".into()
        }
    );
    assert!(b.text("f.txt").contains("<<<<<<<"), "{}", b.text("f.txt"));
    assert_eq!(
        r.ask(Request::Resolve {
            project,
            path,
            done: true
        }),
        Response::Ok
    );
    assert_eq!(r.status().conflicts, 0);
    assert_eq!(r.status().state, SyncState::Synced);
    // requests the daemon answers itself are harmless here
    assert_eq!(r.ask(Request::Status), Response::Ok);
    assert_eq!(r.ask(Request::Shutdown), Response::Ok);
    assert!(
        !r.thread.is_finished(),
        "only WorkerMsg::Shutdown stops the worker"
    );
    r.stop();
}

#[test]
fn restore_by_time_and_from_join_snapshot() {
    let mut w = World::new();
    let a = w.device("a");
    a.write("x", "1");
    let ea = a.create();
    let project = ea.project_id;
    let r = Running::start(ea, LONG);
    a.write("x", "changed");
    let latest = RestoreSpec::Before { time: i64::MAX };
    let req = |from| Request::Restore {
        project,
        path: "x".into(),
        from,
    };
    assert_eq!(r.ask(req(latest)), Response::Ok);
    assert_eq!(a.text("x"), "1");
    let none_before = error_text(r.ask(req(RestoreSpec::Before { time: 0 })));
    assert_eq!(none_before, "no local snapshot at or before 0");
    let not_joined = error_text(r.ask(req(RestoreSpec::Join)));
    assert_eq!(not_joined, "this machine has no join snapshot");
    r.stop();
}

/// Records project messages and turns each into two volume prompts, like the Docker extension.
#[derive(Default)]
struct Volumes {
    messages: Mutex<Vec<Vec<u8>>>,
}

impl Extensions for Volumes {
    fn on_message(&self, e: &mut Engine, payload: &[u8]) {
        self.messages.lock().unwrap().push(payload.to_vec());
        for volume in ["db", "cache"] {
            let (project, from_device) = (e.project_id, "b".to_owned());
            let volume = volume.to_owned();
            e.ctx.notifier.notify(Notification::VolumePrompt {
                project,
                volume,
                from_device,
                at: 5,
            });
        }
        let command = "docker compose up".to_owned();
        e.ctx.notifier.notify(Notification::HookPending {
            project: e.project_id,
            command,
        });
    }
    fn handle(&self, _e: &mut Engine, req: &Request) -> Option<Response> {
        matches!(req, Request::VolumePull { .. }).then_some(Response::Ok)
    }
}

/// Panics on any volume request.
struct Broken;

impl Extensions for Broken {
    fn handle(&self, _e: &mut Engine, _req: &Request) -> Option<Response> {
        panic!("extension bug");
    }
}

fn start_daemon(
    d: &Device,
    project: Uuid,
    ext: Arc<dyn Extensions>,
) -> std::thread::JoinHandle<spoko_client::Result<()>> {
    let mut lc = LocalConfig::load(&d.home).unwrap();
    lc.projects.insert(
        project,
        LocalProject {
            path: d.root.clone(),
            paused: false,
        },
    );
    lc.save(&d.home).unwrap();
    let home = d.home.clone();
    let opts = DaemonOpts {
        ext,
        notifier: d.notes.clone(),
        env: d.ctx.env.clone(),
        rescan_every: LONG,
    };
    let h = std::thread::spawn(move || daemon::run(&home, opts));
    wait_for(Duration::from_secs(10), || {
        ipc::request(&d.home, &Request::Status).is_ok()
    });
    h
}

fn stop_daemon(d: &Device, h: std::thread::JoinHandle<spoko_client::Result<()>>) {
    assert_eq!(
        ipc::request(&d.home, &Request::Shutdown).unwrap(),
        Response::Ok
    );
    h.join().unwrap().unwrap();
}

fn prompts(d: &Device) -> Vec<String> {
    match ipc::request(&d.home, &Request::Status).unwrap() {
        Response::Status { report } => report.projects[0]
            .volume_prompts
            .iter()
            .map(|p| p.volume.clone())
            .collect(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn project_messages_reach_extensions_and_volume_prompts_show_in_status() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    let c = w.device("c");
    a.write("x", "1");
    let project = a.create().project_id;
    drop(b.join(project));
    let vols = Arc::new(Volumes::default());
    let ha = start_daemon(&a, project, vols.clone());
    let hb = start_daemon(&b, project, Arc::new(NoExtensions));
    let online = |d: &Device| {
        c.ctx
            .api
            .presence(project)
            .unwrap()
            .online_devices
            .contains(&d.ctx.device_id)
    };
    wait_for(Duration::from_secs(10), || online(&a) && online(&b));
    c.ctx.api.post_message(project, b"volume?").unwrap();
    wait_for(Duration::from_secs(10), || {
        !vols.messages.lock().unwrap().is_empty()
    });
    assert_eq!(*vols.messages.lock().unwrap(), vec![b"volume?".to_vec()]);
    assert_eq!(prompts(&a), ["db", "cache"]);
    let seen = a.notes.take();
    assert_eq!(
        seen.iter()
            .filter(|n| matches!(n, Notification::VolumePrompt { .. }))
            .count(),
        2
    );
    assert!(
        seen.contains(&Notification::HookPending {
            project,
            command: "docker compose up".into()
        }),
        "other notifications pass through untracked"
    );
    // a daemon without extensions ignores the message and keeps working
    assert!(prompts(&b).is_empty());
    let pull = |volume: Option<&str>| Request::VolumePull {
        project,
        volume: volume.map(str::to_owned),
        force: false,
        fresh: false,
    };
    assert_eq!(
        ipc::request(&a.home, &pull(Some("db"))).unwrap(),
        Response::Ok
    );
    assert_eq!(prompts(&a), ["cache"]);
    assert_eq!(ipc::request(&a.home, &pull(None)).unwrap(), Response::Ok);
    assert!(prompts(&a).is_empty());
    stop_daemon(&a, ha);
    stop_daemon(&b, hb);
}

#[test]
fn panicking_extension_stops_only_its_project_worker() {
    let mut w = World::new();
    let a = w.device("a");
    a.write("x", "1");
    let project = a.create().project_id;
    let h = start_daemon(&a, project, Arc::new(Broken));
    let push = Request::VolumePush {
        project,
        volume: None,
    };
    let stopped = Response::Error {
        message: "project worker stopped".into(),
    };
    assert_eq!(ipc::request(&a.home, &push).unwrap(), stopped);
    std::thread::sleep(Duration::from_millis(200)); // the worker thread finishes unwinding
    assert_eq!(
        ipc::request(&a.home, &Request::Sync { project }).unwrap(),
        stopped
    );
    assert!(matches!(
        ipc::request(&a.home, &Request::Status).unwrap(),
        Response::Status { .. }
    ));
    stop_daemon(&a, h);
}

#[test]
fn only_project_requests_name_a_project() {
    let p = Uuid::new_v4();
    assert_eq!(Request::Status.project(), None);
    assert_eq!(Request::Shutdown.project(), None);
    let pull = Request::VolumePull {
        project: p,
        volume: None,
        force: true,
        fresh: true,
    };
    assert_eq!(pull.project(), Some(p));
    assert_eq!(
        Request::Log {
            project: p,
            path: None,
            limit: 1
        }
        .project(),
        Some(p)
    );
}

#[test]
fn default_home_follows_spoko_home() {
    let home = ipc::default_home();
    match std::env::var_os("SPOKO_HOME").filter(|v| !v.is_empty()) {
        Some(v) => assert_eq!(home, PathBuf::from(v)),
        None => assert_eq!(home.file_name().unwrap(), ".spoko"),
    }
}

#[cfg(unix)]
#[test]
fn ipc_server_answers_bad_requests_and_survives_broken_connections() {
    use interprocess::local_socket::prelude::*;
    use interprocess::local_socket::{GenericFilePath, Stream};
    use std::io::{BufRead, BufReader, Write};

    let home = tempfile::tempdir().unwrap();
    let listener = ipc::listen(home.path()).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let server = {
        let seen = seen.clone();
        std::thread::spawn(move || {
            ipc::serve(&listener, &mut |req| {
                seen.lock().unwrap().push(req);
                Response::Text {
                    text: "handled".into(),
                }
            });
        })
    };
    let raw = |bytes: &[u8]| -> String {
        let name = ipc::socket_path(home.path())
            .to_fs_name::<GenericFilePath>()
            .unwrap();
        let mut conn = BufReader::new(Stream::connect(name).unwrap());
        conn.get_mut().write_all(bytes).unwrap();
        let mut line = String::new();
        conn.read_line(&mut line).unwrap();
        line
    };
    let bad: Response = serde_json::from_str(&raw(b"{\"cmd\":\"fly\"}\n")).unwrap();
    assert!(
        matches!(&bad, Response::Error { message } if message.starts_with("bad request")),
        "{bad:?}"
    );
    assert_eq!(
        raw(b"\xff\xfe\n"),
        "",
        "unreadable request: connection closed without an answer"
    );
    let ok = ipc::request(home.path(), &Request::Status).unwrap();
    assert_eq!(
        ok,
        Response::Text {
            text: "handled".into()
        }
    );
    assert_eq!(
        ipc::request(home.path(), &Request::Shutdown).unwrap(),
        Response::Ok
    );
    server.join().unwrap();
    assert_eq!(*seen.lock().unwrap(), vec![Request::Status]);
}

#[test]
fn watcher_errors_force_a_rescan_of_every_root() {
    let roots = vec![PathBuf::from("/p1"), PathBuf::from("/p2")];
    let (raw_tx, raw_rx) = unbounded();
    let (out_tx, out_rx) = unbounded();
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| {
        let lp = s.spawn(|| debounce_loop(&roots, &raw_rx, &out_tx, &stop));
        raw_tx
            .send(Err(notify::Error::generic("event queue overflow")))
            .unwrap();
        let mut got: Vec<_> = (0..2)
            .map(|_| out_rx.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        got.sort_by(|x, y| x.root.cmp(&y.root));
        assert_eq!(
            got.iter().map(|b| b.root.clone()).collect::<Vec<_>>(),
            roots
        );
        assert!(got.iter().all(|b| b.rescan && b.paths.is_empty()));
        drop(raw_tx); // the OS watcher went away: the loop ends on its own
        lp.join().unwrap();
    });
}

#[test]
fn watcher_loop_ends_when_nobody_listens() {
    let root = PathBuf::from("/p");
    let roots = vec![root.clone()];
    let (raw_tx, raw_rx) = unbounded();
    let (out_tx, out_rx) = unbounded();
    drop(out_rx);
    let stop = AtomicBool::new(false);
    std::thread::scope(|s| {
        let lp = s.spawn(|| debounce_loop(&roots, &raw_rx, &out_tx, &stop));
        let ev = notify::Event::new(notify::EventKind::Any).add_path(root.join("f"));
        raw_tx.send(Ok(ev)).unwrap();
        wait_for(Duration::from_secs(5), || lp.is_finished());
        lp.join().unwrap();
    });
    drop(raw_tx);
}
