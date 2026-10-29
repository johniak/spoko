//! Pure menu model built from the daemon status (testable without a GUI).

use std::path::PathBuf;

use spoko_client::daemon::ipc::{Request, StatusReport, SyncState};

/// Ordered from best to worst; the icon shows the worst project state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum IconState {
    Ok,
    Syncing,
    Paused,
    Offline,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Ipc(Request),
    /// Opens a terminal in `cwd` running `command` (interactive steps: resolve, confirmations).
    Terminal { cwd: PathBuf, command: String },
    StartDaemon,
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    pub label: String,
    pub action: Option<Action>,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuModel {
    pub icon: IconState,
    pub tooltip: String,
    pub items: Vec<MenuItem>,
}

fn info(label: impl Into<String>) -> MenuItem {
    MenuItem { label: label.into(), action: None, enabled: false }
}

fn act(label: impl Into<String>, action: Action) -> MenuItem {
    MenuItem { label: label.into(), action: Some(action), enabled: true }
}

pub const SEPARATOR: &str = "—";

fn state_icon(s: &SyncState) -> IconState {
    match s {
        SyncState::Synced => IconState::Ok,
        SyncState::Syncing => IconState::Syncing,
        SyncState::Paused(_) => IconState::Paused,
        SyncState::Offline => IconState::Offline,
        SyncState::Conflict(_) => IconState::Conflict,
    }
}

fn state_label(s: &SyncState) -> String {
    match s {
        SyncState::Synced => "zsynchronizowany".into(),
        SyncState::Syncing => "synchronizuje…".into(),
        SyncState::Paused(r) => format!("wstrzymany ({r})"),
        SyncState::Offline => "offline".into(),
        SyncState::Conflict(n) => format!("konflikty: {n}"),
    }
}

pub fn build(r: &StatusReport) -> MenuModel {
    if r.locked {
        let home = std::env::home_dir().unwrap_or_else(std::env::temp_dir);
        return MenuModel {
            icon: IconState::Paused,
            tooltip: "spoko: zablokowany — podaj hasło klucza SSH".into(),
            items: vec![
                act("Odblokuj (hasło klucza SSH)", Action::Terminal { cwd: home, command: "spoko unlock".into() }),
                act("Zakończ", Action::Quit),
            ],
        };
    }
    let icon = r.projects.iter().map(|p| state_icon(&p.state)).max().unwrap_or(IconState::Ok);
    let mut items = vec![info(format!("spoko działa · {} · {}", r.device_name, projects_pl(r.projects.len()))), info(SEPARATOR)];
    for p in &r.projects {
        let id = p.id;
        items.push(info(format!("{} — {}", p.name, state_label(&p.state))));
        for h in &p.pending_hooks {
            items.push(act(format!("Uruchom: {h}"), Action::Ipc(Request::HooksRun { project: id })));
        }
        for v in &p.volume_prompts {
            items.push(act(format!("Pobierz {} z {}", v.volume, v.from_device), Action::Ipc(Request::VolumePull { project: id, volume: Some(v.volume.clone()), force: false, fresh: true })));
        }
        if p.conflicts > 0 {
            items.push(act(format!("Rozwiąż konflikty ({})", p.conflicts), Action::Terminal { cwd: p.root.clone(), command: "spoko resolve".into() }));
        }
        match &p.state {
            SyncState::Paused(reason) if reason.starts_with("mass delete") => {
                items.push(act("Wznów (sprawdź usunięcia)", Action::Terminal { cwd: p.root.clone(), command: "spoko status".into() }));
            }
            SyncState::Paused(_) => items.push(act("Wznów", Action::Ipc(Request::Resume { project: id, confirm: false }))),
            _ => items.push(act("Pauza", Action::Ipc(Request::Pause { project: id }))),
        }
        items.push(act("Synchronizuj teraz", Action::Ipc(Request::Sync { project: id })));
        items.push(info(SEPARATOR));
    }
    let tooltip = match icon {
        IconState::Ok => "spoko: wszystko zsynchronizowane",
        IconState::Syncing => "spoko: synchronizuje",
        IconState::Paused => "spoko: wstrzymany projekt",
        IconState::Offline => "spoko: offline — zmiany zapisywane lokalnie",
        IconState::Conflict => "spoko: konflikt do rozwiązania",
    };
    items.push(act("Zakończ", Action::Quit));
    MenuModel { icon, tooltip: tooltip.into(), items }
}

fn projects_pl(n: usize) -> String {
    match n {
        0 => "brak projektów (spoko init)".into(),
        1 => "1 projekt".into(),
        n if (2..=4).contains(&(n % 10)) && !(12..=14).contains(&(n % 100)) => format!("{n} projekty"),
        n => format!("{n} projektów"),
    }
}

pub fn build_unreachable() -> MenuModel {
    MenuModel {
        icon: IconState::Offline,
        tooltip: "spoko: daemon nie działa".into(),
        items: vec![info("Daemon nie działa"), act("Uruchom daemona", Action::StartDaemon), act("Zakończ", Action::Quit)],
    }
}
