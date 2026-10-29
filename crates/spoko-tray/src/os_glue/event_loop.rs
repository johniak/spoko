//! `tao` event loop + `tray-icon`: polls the daemon every 2 s and maps clicks to actions.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use spoko_client::daemon::ipc::{self, Request, Response};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem as TrayItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::menu::{Action, IconState, MenuModel, SEPARATOR, build, build_unreachable};

const POLL: Duration = Duration::from_secs(2);

fn icon(state: IconState) -> Icon {
    let (r, g, b) = match state {
        IconState::Ok => (46, 160, 67),
        IconState::Syncing => (56, 132, 255),
        IconState::Paused => (150, 150, 150),
        IconState::Offline => (210, 150, 30),
        IconState::Conflict => (220, 50, 47),
    };
    const N: u32 = 32;
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    for y in 0..N {
        for x in 0..N {
            let (dx, dy) = (x as f32 - 15.5, y as f32 - 15.5);
            let inside = dx * dx + dy * dy <= 13.0 * 13.0;
            rgba.extend_from_slice(&[r, g, b, if inside { 255 } else { 0 }]);
        }
    }
    Icon::from_rgba(rgba, N, N).expect("valid icon")
}

fn model(home: &Path) -> MenuModel {
    match ipc::request(home, &Request::Status) {
        Ok(Response::Status { report }) => build(&report),
        _ => build_unreachable(),
    }
}

fn menu_for(m: &MenuModel) -> (Menu, HashMap<MenuId, Action>) {
    let menu = Menu::new();
    let mut actions = HashMap::new();
    for item in &m.items {
        if item.label == SEPARATOR {
            let _ = menu.append(&PredefinedMenuItem::separator());
            continue;
        }
        let t = TrayItem::new(&item.label, item.enabled, None);
        if let Some(a) = &item.action {
            actions.insert(t.id().clone(), a.clone());
        }
        let _ = menu.append(&t);
    }
    (menu, actions)
}

fn open_terminal(cwd: &Path, command: &str) {
    let dir = cwd.to_string_lossy();
    let _ = if cfg!(target_os = "macos") {
        let script = format!("tell application \"Terminal\" to do script \"cd '{dir}' && {command}\"");
        Command::new("osascript").args(["-e", &script, "-e", "tell application \"Terminal\" to activate"]).spawn()
    } else if cfg!(windows) {
        Command::new("cmd").args(["/C", "start", "cmd", "/K", command]).current_dir(cwd).spawn()
    } else {
        Command::new("x-terminal-emulator").args(["-e", "sh", "-c", &format!("{command}; exec sh")]).current_dir(cwd).spawn()
    };
}

/// `spoko` next to this executable (both come from the same install), else from `PATH`.
fn spoko_exe() -> std::path::PathBuf {
    let name = format!("spoko{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe().ok().map(|e| e.with_file_name(&name)).filter(|p| p.is_file()).unwrap_or_else(|| name.into())
}

pub fn run(home: std::path::PathBuf) {
    // one icon per user: a second tray (another `spoko daemon` start) leaves at once
    let _ = std::fs::create_dir_all(&home);
    let Ok(file) = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(home.join("tray.lock")) else { return };
    let lock = Box::leak(Box::new(fd_lock::RwLock::new(file)));
    let Ok(guard) = lock.try_write() else { return };
    std::mem::forget(guard);
    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::new().build();
    #[cfg(target_os = "macos")]
    {
        // a menu-bar item only: no Dock icon, no app switcher entry
        use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
    }
    let mut tray: Option<TrayIcon> = None;
    let mut current: Option<MenuModel> = None;
    let mut actions: HashMap<MenuId, Action> = HashMap::new();
    let menu_rx = MenuEvent::receiver();
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(250));
        if let Event::NewEvents(StartCause::Init) = event {
            // macOS: the status item must be created once the loop runs
            tray = TrayIconBuilder::new().with_icon(icon(IconState::Offline)).with_tooltip("spoko").build().ok();
        }
        while let Ok(ev) = menu_rx.try_recv() {
            match actions.get(&ev.id) {
                Some(Action::Quit) => *flow = ControlFlow::Exit,
                Some(Action::Ipc(req)) => {
                    let _ = ipc::request(&home, req);
                    current = None;
                }
                Some(Action::Terminal { cwd, command }) => open_terminal(cwd, command),
                Some(Action::StartDaemon) => {
                    let _ = Command::new(spoko_exe()).args(["daemon", "start"]).spawn();
                }
                None => {}
            }
        }
        static LAST: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
        let mut last = LAST.lock().expect("poll clock");
        if last.is_none_or(|t| t.elapsed() >= POLL) || current.is_none() {
            *last = Some(Instant::now());
            let m = model(&home);
            if current.as_ref() != Some(&m)
                && let Some(t) = &tray
            {
                let (menu, acts) = menu_for(&m);
                t.set_menu(Some(Box::new(menu)));
                let _ = t.set_icon(Some(icon(m.icon)));
                let _ = t.set_tooltip(Some(&m.tooltip));
                actions = acts;
                current = Some(m);
            }
        }
    })
}
