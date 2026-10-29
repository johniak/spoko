use std::io::Write;
use std::path::PathBuf;

use spoko_client::daemon::ipc::{LogLine, ProjectStatus, Request, Response, RestoreSpec, StatusReport, SyncState};
use spoko_client::engine::{Engine, PauseReason, SyncOutcome};
use spoko_client::history::{self, RestoreFrom};
use spoko_client::hooks;
use spoko_client::resolve::{self, MergeTool, ResolveOutcome};
use spoko_core::platform::RelPath;
use uuid::Uuid;

use super::{Ctx, Res, err, fmt_time};
use crate::cli::{HooksCmd, VolumeCmd};

fn canonical_dir(ctx: &Ctx, path: Option<PathBuf>) -> Res<PathBuf> {
    let p = match path {
        Some(p) if p.is_absolute() => p,
        Some(p) => ctx.cwd.join(p),
        None => ctx.cwd.clone(),
    };
    std::fs::create_dir_all(&p)?;
    // Claude Code keys chats by the physical path: resolve symlinks (/var → /private/var)
    Ok(p.canonicalize()?)
}

pub fn init(ctx: &Ctx, path: Option<PathBuf>, name: Option<String>, out: &mut dyn Write) -> Res<()> {
    let root = canonical_dir(ctx, path)?;
    if ctx.local()?.projects.values().any(|p| p.path == root) {
        return err(format!("{} już jest projektem spoko", root.display()));
    }
    let id = ctx.identity()?;
    // only `/` has no file name
    let name = name.unwrap_or_else(|| root.file_name().map_or("projekt".into(), |n| n.to_string_lossy().into_owned()));
    let e = Engine::create(ctx.device_ctx(&id), &root, &name)?;
    ctx.register(e.project_id, &root)?;
    writeln!(out, "Projekt: {}", e.project_id)?;
    writeln!(out, "Na innym komputerze: spoko clone {} <katalog>", e.project_id)?;
    Ok(())
}

pub fn clone(ctx: &Ctx, project: &str, path: Option<PathBuf>, out: &mut dyn Write) -> Res<()> {
    let project: Uuid = project.parse().map_err(|_| format!("„{project}” to nie jest identyfikator projektu"))?;
    let root = canonical_dir(ctx, path)?;
    let id = ctx.identity()?;
    let (_e, report) = Engine::clone_into(ctx.device_ctx(&id), project, &root)?;
    ctx.register(project, &root)?;
    writeln!(out, "Projekt: {project}\nKatalog: {}", root.display())?;
    if !report.differing.is_empty() {
        writeln!(out, "Lokalne różnice ({} plików) zachowano w snapshocie dołączenia; przywrócisz je: spoko restore <plik> --from-join", report.differing.len())?;
    }
    Ok(())
}

fn outcome_pl(o: SyncOutcome) -> String {
    match o {
        SyncOutcome::UpToDate => "aktualny".into(),
        SyncOutcome::Pushed => "wysłano".into(),
        SyncOutcome::FastForwarded => "pobrano".into(),
        SyncOutcome::Merged { conflicts: 0 } => "scalono".into(),
        SyncOutcome::Merged { conflicts } => format!("scalono, konflikty: {conflicts} (spoko resolve)"),
        SyncOutcome::Paused => "wstrzymany (spoko status)".into(),
    }
}

pub fn sync(ctx: &Ctx, out: &mut dyn Write) -> Res<()> {
    let targets: Vec<(Uuid, PathBuf)> = match ctx.current_project() {
        Ok(p) => vec![p],
        Err(_) => ctx.local()?.projects.into_iter().map(|(id, p)| (id, p.path)).collect(),
    };
    let daemon = ctx.daemon_running();
    for (project, root) in targets {
        let name = root.display().to_string();
        if daemon {
            ctx.via_daemon(&Request::Sync { project })?;
            writeln!(out, "{name}: ok")?;
        } else {
            let o = ctx.with_engine(project, &root, |e| Ok(e.sync()?))?;
            writeln!(out, "{name}: {}", outcome_pl(o))?;
        }
    }
    Ok(())
}

fn offline_status(ctx: &Ctx, id: Uuid, root: &std::path::Path) -> Res<ProjectStatus> {
    let ident = ctx.identity()?;
    let e = Engine::open(ctx.device_ctx(&ident), id, root)?;
    let conflicts = resolve::list(&e)?.len();
    let state = match &e.state.paused {
        Some(PauseReason::User) => SyncState::Paused("paused".into()),
        Some(PauseReason::MassDelete { deleted, total }) => SyncState::Paused(format!("mass delete {deleted}/{total}")),
        Some(PauseReason::MissingRoot) => SyncState::Paused("project directory missing".into()),
        None if conflicts > 0 => SyncState::Conflict(conflicts),
        None if e.state.base == e.state.last_local => SyncState::Synced,
        None => SyncState::Syncing,
    };
    let pending_hooks = e.state.pending_hooks.iter().filter_map(|h| e.config().watch.get(*h)).filter_map(|w| hooks::command_for(w, hooks::Os::current())).map(str::to_owned).collect();
    let name = root.file_name().map_or(id.to_string(), |n| n.to_string_lossy().into_owned());
    Ok(ProjectStatus { id, name, root: root.to_path_buf(), state, last_sync: None, pending_hooks, conflicts, warnings: vec![], volume_prompts: vec![] })
}

fn state_pl(s: &SyncState) -> String {
    match s {
        SyncState::Synced => "zsynchronizowany".into(),
        SyncState::Syncing => "synchronizuje / zmiany czekają".into(),
        SyncState::Conflict(n) => format!("konflikty: {n} (spoko resolve)"),
        SyncState::Offline => "offline (zmiany zapisane lokalnie)".into(),
        SyncState::Paused(r) => format!("wstrzymany: {r} (spoko resume)"),
    }
}

pub fn status(ctx: &Ctx, json: bool, out: &mut dyn Write) -> Res<()> {
    let id = ctx.identity()?;
    let (running, report) = match ctx.via_daemon(&Request::Status)? {
        Some(Response::Status { report }) => (true, report),
        _ => {
            let mut projects = Vec::new();
            for (pid, p) in ctx.local()?.projects {
                projects.push(offline_status(ctx, pid, &p.path)?);
            }
            (false, StatusReport { device_name: id.device_name.clone(), projects })
        }
    };
    if json {
        writeln!(out, "{}", serde_json::to_string_pretty(&report)?)?;
        return Ok(());
    }
    writeln!(out, "urządzenie: {}  daemon: {}", report.device_name, if running { "działa" } else { "nie działa" })?;
    for p in &report.projects {
        writeln!(out, "{}  {}  {}", p.name, p.root.display(), state_pl(&p.state))?;
        if let Some(t) = p.last_sync {
            writeln!(out, "  ostatnia synchronizacja: {}", fmt_time(t))?;
        }
        for h in &p.pending_hooks {
            writeln!(out, "  czeka hook: {h} (spoko hooks run)")?;
        }
        for w in &p.warnings {
            writeln!(out, "  uwaga: {w}")?;
        }
    }
    Ok(())
}

pub fn resolve(ctx: &Ctx, path: Option<String>, done: bool, out: &mut dyn Write) -> Res<()> {
    let (project, root) = ctx.current_project()?;
    let Some(path) = path else {
        let id = ctx.identity()?;
        let e = Engine::open(ctx.device_ctx(&id), project, &root)?;
        let list = resolve::list(&e)?;
        if list.is_empty() {
            writeln!(out, "Brak konfliktów.")?;
        }
        for c in list {
            writeln!(out, "{}  (druga wersja z {}, {})", c.path.strip_prefix("work/").unwrap_or(&c.path), c.theirs_device, fmt_time(c.at))?;
        }
        return Ok(());
    };
    let rel = rel_in_project(ctx, &root, &path)?;
    if let Some(r) = ctx.via_daemon(&Request::Resolve { project, path: rel.to_string(), done })? {
        writeln!(out, "{}", if let Response::Text { text } = r { text } else { "ok".into() })?;
        return Ok(());
    }
    ctx.with_engine(project, &root, |e| {
        if done {
            resolve::mark_resolved(e, &rel)?;
            writeln!(out, "Oznaczono jako rozwiązany: {rel}")?;
        } else {
            match resolve::resolve(e, &rel, &MergeTool::from_git_config(&root))? {
                ResolveOutcome::Resolved => writeln!(out, "Rozwiązano: {rel}")?,
                ResolveOutcome::Unchanged => writeln!(out, "Plik zawiera wersje do scalenia; po edycji: spoko resolve {rel} --done")?,
            }
        }
        Ok(())
    })
}

/// A path argument relative to the current directory, as a project path.
fn rel_in_project(ctx: &Ctx, root: &std::path::Path, path: &str) -> Res<RelPath> {
    let cwd = ctx.cwd.canonicalize().unwrap_or_else(|_| ctx.cwd.clone());
    let abs = cwd.join(path);
    RelPath::from_fs(root, &abs).ok_or_else(|| format!("{path} jest poza projektem").into())
}

pub fn log(ctx: &Ctx, path: Option<String>, limit: usize, json: bool, out: &mut dyn Write) -> Res<()> {
    let (project, root) = ctx.current_project()?;
    let rel = path.as_deref().map(|p| rel_in_project(ctx, &root, p)).transpose()?;
    let entries: Vec<LogLine> = match ctx.via_daemon(&Request::Log { project, path: rel.as_ref().map(ToString::to_string), limit })? {
        Some(Response::Log { entries }) => entries,
        _ => {
            let id = ctx.identity()?;
            let e = Engine::open(ctx.device_ctx(&id), project, &root)?;
            history::log(&e, rel.as_ref(), limit)?.into_iter().map(|l| LogLine { commit: l.commit.to_string(), time: l.time, device_name: l.device_name, changed: l.changed }).collect()
        }
    };
    let entries: Vec<LogLine> = if rel.is_some() { entries.into_iter().filter(|l| l.changed).collect() } else { entries };
    if json {
        writeln!(out, "{}", serde_json::to_string_pretty(&entries)?)?;
    } else {
        for l in entries {
            writeln!(out, "{}  {}  {}", &l.commit[..12], fmt_time(l.time), l.device_name)?;
        }
    }
    Ok(())
}

/// `<40 hex>` commit, unix seconds, or `10m` / `2h` / `3d` ago.
pub fn parse_at(at: &str, now: i64) -> Res<RestoreSpec> {
    if at.len() == 40 && at.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(RestoreSpec::Commit { oid: at.to_owned() });
    }
    if let Ok(t) = at.parse::<i64>() {
        return Ok(RestoreSpec::Before { time: t });
    }
    let (num, unit) = at.split_at(at.len().saturating_sub(1));
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86_400,
        _ => return err(format!("nie rozumiem --at {at} (commit, czas unix albo np. 10m, 2h, 3d)")),
    };
    let n: i64 = num.parse().map_err(|_| format!("nie rozumiem --at {at}"))?;
    Ok(RestoreSpec::Before { time: now - n * mult })
}

pub fn restore(ctx: &Ctx, path: &str, at: Option<&str>, from_join: bool, out: &mut dyn Write) -> Res<()> {
    let (project, root) = ctx.current_project()?;
    let rel = rel_in_project(ctx, &root, path)?;
    let spec = match (at, from_join) {
        (_, true) => RestoreSpec::Join,
        (Some(a), false) => parse_at(a, spoko_client::engine::unix_now())?,
        (None, false) => return err("podaj --at <commit|czas> albo --from-join"),
    };
    if ctx.via_daemon(&Request::Restore { project, path: rel.to_string(), from: spec.clone() })?.is_none() {
        ctx.with_engine(project, &root, |e| {
            let from = match spec {
                RestoreSpec::Commit { oid } => RestoreFrom::Commit(git2::Oid::from_str(&oid)?),
                RestoreSpec::Before { time } => RestoreFrom::Before(time),
                RestoreSpec::Join => RestoreFrom::Join,
            };
            Ok(history::restore(e, &rel, from)?)
        })?;
    }
    writeln!(out, "Przywrócono {rel}")?;
    Ok(())
}

pub fn pause(ctx: &Ctx, out: &mut dyn Write) -> Res<()> {
    let (project, root) = ctx.current_project()?;
    if ctx.via_daemon(&Request::Pause { project })?.is_none() {
        ctx.with_engine(project, &root, |e| Ok(e.pause()?))?;
    }
    writeln!(out, "Wstrzymano synchronizację {}", root.display())?;
    Ok(())
}

pub fn resume(ctx: &Ctx, confirm: bool, out: &mut dyn Write) -> Res<()> {
    let (project, root) = ctx.current_project()?;
    let r = match ctx.via_daemon(&Request::Resume { project, confirm }) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => ctx.with_engine(project, &root, |e| Ok(e.resume(confirm)?)),
        Err(e) => Err(e),
    };
    r.map_err(|e| if e.to_string().contains("confirm") { format!("{e} — uruchom: spoko resume --confirm").into() } else { e })?;
    writeln!(out, "Wznowiono synchronizację {}", root.display())?;
    Ok(())
}

pub fn volume(ctx: &Ctx, cmd: VolumeCmd, out: &mut dyn Write) -> Res<()> {
    let (project, root) = ctx.current_project()?;
    let (name, pull) = match cmd {
        VolumeCmd::Push { name } => (name, None),
        VolumeCmd::Pull { name, fresh, force } => (name, Some((force, fresh))),
    };
    let req = match pull {
        None => Request::VolumePush { project, volume: name.clone() },
        Some((force, fresh)) => Request::VolumePull { project, volume: name.clone(), force, fresh },
    };
    let text = match ctx.via_daemon(&req)? {
        Some(Response::Text { text }) => text,
        Some(_) => "ok".into(),
        None => ctx.with_engine(project, &root, |e| Ok(crate::os_glue::docker_ext().run_volumes(e, name.as_deref(), pull)?))?,
    };
    writeln!(out, "{text}")?;
    Ok(())
}

pub fn hooks(ctx: &Ctx, cmd: HooksCmd, out: &mut dyn Write) -> Res<()> {
    let (project, root) = ctx.current_project()?;
    match cmd {
        HooksCmd::Run => {
            let runs = match ctx.via_daemon(&Request::HooksRun { project })? {
                Some(Response::Hooks { runs }) => runs,
                _ => ctx.with_engine(project, &root, |e| Ok(hooks::run_pending(e)?))?,
            };
            if runs.is_empty() {
                writeln!(out, "Brak oczekujących hooków.")?;
            }
            for r in runs {
                writeln!(out, "$ {}  → kod {}\n{}", r.command, r.status, r.output_tail.trim_end())?;
            }
        }
        HooksCmd::Dismiss => {
            // through the daemon when it runs: it owns the state and would write the hooks back
            if ctx.via_daemon(&Request::HooksDismiss { project })?.is_none() {
                ctx.with_engine(project, &root, |e| {
                    e.state.pending_hooks.clear();
                    Ok(e.save()?)
                })?;
            }
            writeln!(out, "Pominięto oczekujące hooki.")?;
        }
    }
    Ok(())
}
