use std::path::PathBuf;

use spoko_client::daemon::ipc::{ProjectStatus, Request, StatusReport, SyncState, VolumePrompt};
use spoko_tray::menu::{Action, IconState, build, build_unreachable};
use uuid::Uuid;

fn project(name: &str, state: SyncState) -> ProjectStatus {
    ProjectStatus {
        id: Uuid::new_v4(),
        name: name.into(),
        root: PathBuf::from(format!("/code/{name}")),
        state,
        last_sync: Some(1),
        pending_hooks: vec![],
        conflicts: 0,
        warnings: vec![],
        volume_prompts: vec![],
    }
}

fn report(ps: Vec<ProjectStatus>) -> StatusReport {
    StatusReport { device_name: "mac".into(), projects: ps, locked: false, problem: None }
}

#[test]
fn icon_is_worst_state() {
    assert_eq!(build(&report(vec![])).icon, IconState::Ok);
    assert_eq!(build(&report(vec![project("a", SyncState::Synced), project("b", SyncState::Syncing)])).icon, IconState::Syncing);
    assert_eq!(build(&report(vec![project("a", SyncState::Paused("x".into())), project("b", SyncState::Syncing)])).icon, IconState::Paused);
    assert_eq!(build(&report(vec![project("a", SyncState::Offline), project("b", SyncState::Paused("x".into()))])).icon, IconState::Offline);
    let mut c = project("c", SyncState::Conflict(2));
    c.conflicts = 2;
    let m = build(&report(vec![project("a", SyncState::Offline), c.clone()]));
    assert_eq!(m.icon, IconState::Conflict);
    let item = m.items.iter().find(|i| i.label == "Rozwiąż konflikty (2)").unwrap();
    assert_eq!(item.action, Some(Action::Terminal { cwd: c.root.clone(), command: "spoko resolve".into() }));
    assert!(m.tooltip.contains("konflikt"));
    assert_eq!(m.items.last().unwrap().action, Some(Action::Quit));
}

#[test]
fn pending_hook_item_runs_hooks() {
    let mut p = project("app", SyncState::Synced);
    p.pending_hooks = vec!["npm ci".into()];
    let m = build(&report(vec![p.clone()]));
    let item = m.items.iter().find(|i| i.label == "Uruchom: npm ci").unwrap();
    assert_eq!(item.action, Some(Action::Ipc(Request::HooksRun { project: p.id })));
    assert!(m.items.iter().any(|i| i.label == "Synchronizuj teraz" && i.action == Some(Action::Ipc(Request::Sync { project: p.id }))));
    assert!(m.items.iter().any(|i| i.label == "Pauza" && i.action == Some(Action::Ipc(Request::Pause { project: p.id }))));
    assert!(m.items.iter().any(|i| i.label.starts_with("app") && !i.enabled));
}

#[test]
fn volume_prompt_item() {
    let mut p = project("app", SyncState::Synced);
    p.volume_prompts = vec![VolumePrompt { volume: "pg_data".into(), from_device: "linux".into(), at: 0 }];
    let m = build(&report(vec![p.clone()]));
    let item = m.items.iter().find(|i| i.label == "Pobierz pg_data z linux").unwrap();
    assert_eq!(item.action, Some(Action::Ipc(Request::VolumePull { project: p.id, volume: Some("pg_data".into()), force: false, fresh: true })));
}

#[test]
fn paused_items() {
    let p = project("app", SyncState::Paused("paused".into()));
    let m = build(&report(vec![p.clone()]));
    assert!(m.items.iter().any(|i| i.label == "Wznów" && i.action == Some(Action::Ipc(Request::Resume { project: p.id, confirm: false }))));
    let md = project("big", SyncState::Paused("mass delete 60/100".into()));
    let m = build(&report(vec![md.clone()]));
    let item = m.items.iter().find(|i| i.label.starts_with("Wznów")).unwrap();
    assert_eq!(item.action, Some(Action::Terminal { cwd: md.root.clone(), command: "spoko status".into() }), "mass delete is confirmed in a terminal");
}

#[test]
fn offline_when_daemon_unreachable() {
    let m = build_unreachable();
    assert_eq!(m.icon, IconState::Offline);
    assert!(m.items.iter().any(|i| i.label.contains("nie działa") && !i.enabled));
    assert!(m.items.iter().any(|i| i.action == Some(Action::StartDaemon)));
    assert_eq!(m.items.last().unwrap().action, Some(Action::Quit));
}

#[test]
fn locked_daemon_offers_unlock_in_a_terminal() {
    let m = build(&StatusReport { device_name: "mac".into(), projects: vec![], locked: true, problem: None });
    assert_eq!(m.icon, IconState::Paused);
    assert!(m.tooltip.contains("hasło klucza SSH"), "{}", m.tooltip);
    let unlock = m.items.iter().find(|i| i.label.starts_with("Odblokuj")).expect("unlock item");
    assert!(matches!(&unlock.action, Some(Action::Terminal { command, .. }) if command == "spoko unlock"));
    assert!(matches!(m.items.last().unwrap().action, Some(Action::Quit)));
}

#[test]
fn first_line_says_spoko_runs_on_this_device() {
    let m = build(&report(vec![project("a", SyncState::Synced), project("b", SyncState::Synced)]));
    assert_eq!(m.items[0].label, "spoko działa · mac · 2 projekty");
    assert!(!m.items[0].enabled && m.items[0].action.is_none());
    assert_eq!(build(&report(vec![project("a", SyncState::Synced)])).items[0].label, "spoko działa · mac · 1 projekt");
    let none = build(&report(vec![]));
    assert_eq!(none.items[0].label, "spoko działa · mac · brak projektów (spoko init)");
    assert_eq!(build(&report((0..5).map(|i| project(&i.to_string(), SyncState::Synced)).collect())).items[0].label, "spoko działa · mac · 5 projektów");
}

#[test]
fn daemon_without_identity_says_why_and_offers_login() {
    let m = build(&StatusReport { device_name: String::new(), projects: vec![], locked: false, problem: Some("not logged in (spoko login)".into()) });
    assert_eq!(m.icon, IconState::Offline);
    assert!(m.tooltip.contains("not logged in"), "{}", m.tooltip);
    assert!(m.items.iter().any(|i| i.label.contains("not logged in") && !i.enabled));
    assert!(matches!(m.items.last().unwrap().action, Some(Action::Quit)));
}
