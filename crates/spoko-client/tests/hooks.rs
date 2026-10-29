mod support;

use spoko_client::hooks::{self, Os};
use spoko_client::notifier::Notification;
use spoko_core::config::{HookMode, WatchCfg};
use support::*;
use uuid::Uuid;

fn watch(run: &str, mode: &str) -> String {
    format!("[[watch]]\nfiles = [\"x.lock\"]\nrun = {run:?}\nmode = \"{mode}\"\n")
}

#[test]
fn os_specific_command_preferred() {
    let w = WatchCfg { files: vec![], run: Some("generic".into()), run_macos: Some("mac".into()), run_linux: None, run_windows: Some("win".into()), mode: HookMode::Ask };
    assert_eq!(hooks::command_for(&w, Os::Macos), Some("mac"));
    assert_eq!(hooks::command_for(&w, Os::Linux), Some("generic"));
    assert_eq!(hooks::command_for(&w, Os::Windows), Some("win"));
    let none = WatchCfg { run: None, ..w.clone() };
    assert_eq!(hooks::command_for(&none, Os::Linux), None);
    assert!(matches!(Os::current(), Os::Macos | Os::Linux | Os::Windows));
}

#[test]
fn auto_runs_immediately() {
    let mut w = World::new();
    let a = w.device("a");
    a.write(".spokoconfig", watch("echo ok > hook.out", "auto"));
    let mut e = a.create();
    a.notes.take();
    hooks::on_remote_change(&mut e, vec![0]).unwrap();
    assert!(a.text("hook.out").contains("ok"));
    assert!(e.state.pending_hooks.is_empty());
    let log = std::fs::read_to_string(e.data_dir().join("hooks.log")).unwrap();
    assert!(log.contains("echo ok > hook.out"), "{log}");
}

#[test]
fn ask_adds_pending_and_notifies() {
    let mut w = World::new();
    let a = w.device("a");
    a.write(".spokoconfig", watch("echo ok > hook.out", "ask"));
    let mut e = a.create();
    a.notes.take();
    hooks::on_remote_change(&mut e, vec![0]).unwrap();
    hooks::on_remote_change(&mut e, vec![0]).unwrap();
    assert_eq!(e.state.pending_hooks, vec![0], "queued once");
    assert_eq!(a.notes.take(), vec![Notification::HookPending { project: e.project_id, command: "echo ok > hook.out".into() }]);
    assert!(a.read("hook.out").is_none());
    let runs = hooks::run_pending(&mut e).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, 0);
    assert!(e.state.pending_hooks.is_empty());
    assert!(a.text("hook.out").contains("ok"));
    assert!(hooks::run_pending(&mut e).unwrap().is_empty());
}

#[test]
fn notify_does_not_add_pending() {
    let mut w = World::new();
    let a = w.device("a");
    a.write(".spokoconfig", watch("echo ok > hook.out", "notify"));
    let mut e = a.create();
    a.notes.take();
    hooks::on_remote_change(&mut e, vec![0]).unwrap();
    assert!(e.state.pending_hooks.is_empty());
    assert_eq!(a.notes.take().len(), 1);
    assert!(a.read("hook.out").is_none());
}

#[test]
fn failed_hook_stays_pending() {
    let mut w = World::new();
    let a = w.device("a");
    a.write(".spokoconfig", watch("echo broken && exit 3", "ask"));
    let mut e = a.create();
    hooks::on_remote_change(&mut e, vec![0, 7]).unwrap();
    let runs = hooks::run_pending(&mut e).unwrap();
    assert_eq!(runs.len(), 1, "unknown hook index is dropped");
    assert_eq!(runs[0].status, 3);
    assert!(runs[0].output_tail.contains("broken"));
    assert_eq!(e.state.pending_hooks, vec![0]);
    // a failing auto hook also stays pending
    a.write(".spokoconfig", watch("exit 1", "auto"));
    e.sync().unwrap();
    e.state.pending_hooks.clear();
    hooks::on_remote_change(&mut e, vec![0]).unwrap();
    assert_eq!(e.state.pending_hooks, vec![0]);
}

#[test]
fn remote_lockfile_change_goes_through_hook_modes() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write(".spokoconfig", "[[watch]]\nfiles = [\"package-lock.json\"]\nrun = \"echo installed > installed.txt\"\nmode = \"auto\"\n");
    a.write("package-lock.json", "1");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    std::fs::remove_file(b.root.join("installed.txt")).ok();
    a.write("package-lock.json", "2");
    ea.sync().unwrap();
    eb.sync().unwrap();
    assert!(b.text("installed.txt").contains("installed"));
    assert!(eb.state.pending_hooks.is_empty());
}

#[test]
fn notification_texts() {
    let p = Uuid::nil();
    let texts: Vec<String> = [
        Notification::Conflict { project: p, path: "work/src/a.rs".into(), from_device: "linux".into() },
        Notification::BinaryOverwritten { project: p, path: "work/logo.png".into(), from_device: "linux".into() },
        Notification::HookPending { project: p, command: "npm ci".into() },
        Notification::VolumePrompt { project: p, volume: "pgdata".into(), from_device: "linux".into(), at: 0 },
        Notification::MassDelete { project: p, deleted: 60, total: 100 },
        Notification::MissingRoot { project: p },
        Notification::PlatformSkip { project: p, path: "aux.txt".into(), reason: "zarezerwowana nazwa".into() },
        Notification::GitBusy { project: p },
    ]
    .iter()
    .map(|n| n.text())
    .collect();
    assert_eq!(
        texts,
        vec![
            "Konflikt w src/a.rs: zostaje Twoja wersja, wersja z linux jest w historii (spoko resolve).",
            "logo.png: nowsza wersja z linux zastąpiła Twoją (starsza jest w historii).",
            "Zmieniły się zależności — uruchom: npm ci (spoko hooks run).",
            "Wolumen pgdata z linux jest nowszy — zastosować? (spoko volume pull)",
            "Synchronizacja wstrzymana: znikło 60 z 100 plików. Potwierdź: spoko resume --confirm",
            "Synchronizacja wstrzymana: katalog projektu zniknął.",
            "Pominięto aux.txt: zarezerwowana nazwa",
            "Git jest zajęty — stan repozytorium zsynchronizuje się po zakończeniu operacji.",
        ]
    );
}
