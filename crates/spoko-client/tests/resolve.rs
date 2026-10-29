mod support;

use spoko_client::engine::SyncOutcome;
use spoko_client::history::{self, RestoreFrom};
use spoko_client::resolve::{self, MergeTool, ResolveOutcome};
use spoko_core::platform::RelPath;
use support::*;

/// A and B edit the same line; B syncs second, so B's version is active and A's is the conflict.
fn conflicted() -> (World, Device, Device, spoko_client::engine::Engine, spoko_client::engine::Engine) {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("f.txt", "base\n");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    a.write("f.txt", "from a\n");
    b.write("f.txt", "from b\n");
    ea.sync().unwrap();
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Merged { conflicts: 1 });
    (w, a, b, ea, eb)
}

#[test]
fn list_shows_conflict() {
    let (_w, _a, _b, _ea, eb) = conflicted();
    let cs = resolve::list(&eb).unwrap();
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].path, "work/f.txt");
}

#[cfg(unix)]
#[test]
fn resolve_with_fake_tool_script() {
    let (_w, _a, b, _ea, mut eb) = conflicted();
    let tool = MergeTool::Command("cp \"$REMOTE\" \"$MERGED\"".into());
    assert_eq!(resolve::resolve(&mut eb, &RelPath::new("f.txt"), &tool).unwrap(), ResolveOutcome::Resolved);
    assert_eq!(b.text("f.txt"), "from a\n");
    eb.sync().unwrap();
    assert!(resolve::list(&eb).unwrap().is_empty());
    // a tool that changes nothing leaves the conflict open
}

#[cfg(windows)]
#[test]
fn resolve_with_fake_tool_script() {
    let (_w, _a, b, _ea, mut eb) = conflicted();
    let tool = MergeTool::Command("copy /Y \"$REMOTE\" \"$MERGED\"".into());
    assert_eq!(resolve::resolve(&mut eb, &RelPath::new("f.txt"), &tool).unwrap(), ResolveOutcome::Resolved);
    assert_eq!(b.text("f.txt"), "from a\n");
}

#[test]
fn tool_that_changes_nothing_is_unchanged() {
    let (_w, _a, b, _ea, mut eb) = conflicted();
    let noop = if cfg!(windows) { "exit 0" } else { "true" };
    assert_eq!(resolve::resolve(&mut eb, &RelPath::new("f.txt"), &MergeTool::Command(noop.into())).unwrap(), ResolveOutcome::Unchanged);
    assert_eq!(b.text("f.txt"), "from b\n");
    assert_eq!(resolve::list(&eb).unwrap().len(), 1);
    assert!(resolve::resolve(&mut eb, &RelPath::new("nope.txt"), &MergeTool::Markers).is_err());
    let failing = if cfg!(windows) { "exit 3" } else { "false" };
    assert!(resolve::resolve(&mut eb, &RelPath::new("f.txt"), &MergeTool::Command(failing.into())).is_err());
}

#[test]
fn markers_mode_writes_markers_then_done() {
    let (_w, _a, b, _ea, mut eb) = conflicted();
    assert_eq!(resolve::resolve(&mut eb, &RelPath::new("f.txt"), &MergeTool::Markers).unwrap(), ResolveOutcome::Unchanged);
    let t = b.text("f.txt");
    assert!(t.contains("<<<<<<<") && t.contains("from a") && t.contains("from b") && t.contains(">>>>>>>"), "{t}");
    b.write("f.txt", "hand merged\n");
    resolve::mark_resolved(&mut eb, &RelPath::new("f.txt")).unwrap();
    eb.sync().unwrap();
    assert!(resolve::list(&eb).unwrap().is_empty());
}

#[test]
fn resolve_on_other_device_clears_conflict_everywhere() {
    let (_w, a, _b, mut ea, mut eb) = conflicted();
    ea.sync().unwrap();
    assert_eq!(resolve::list(&ea).unwrap().len(), 1, "conflict visible on A too");
    resolve::mark_resolved(&mut eb, &RelPath::new("f.txt")).unwrap();
    eb.sync().unwrap();
    // A made an unrelated change meanwhile: the merge must not resurrect the resolved conflict
    a.write("other.txt", "x");
    assert!(matches!(ea.sync().unwrap(), SyncOutcome::Merged { .. }));
    assert!(resolve::list(&ea).unwrap().is_empty());
    eb.sync().unwrap();
    assert!(resolve::list(&eb).unwrap().is_empty());
}

#[test]
fn merge_tool_from_git_config() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("cfg");
    std::fs::write(&p, "[merge]\n\ttool = fake\n[mergetool \"fake\"]\n\tcmd = fake $BASE $LOCAL $REMOTE $MERGED\n").unwrap();
    assert_eq!(MergeTool::from_config(&git2::Config::open(&p).unwrap()), MergeTool::Command("fake $BASE $LOCAL $REMOTE $MERGED".into()));
    std::fs::write(&p, "[merge]\n\ttool = vscode\n").unwrap();
    assert_eq!(MergeTool::from_config(&git2::Config::open(&p).unwrap()), MergeTool::Command("code --wait --merge \"$REMOTE\" \"$LOCAL\" \"$BASE\" \"$MERGED\"".into()));
    std::fs::write(&p, "[merge]\n\ttool = unknown\n").unwrap();
    assert_eq!(MergeTool::from_config(&git2::Config::open(&p).unwrap()), MergeTool::Markers);
    std::fs::write(&p, "").unwrap();
    assert_eq!(MergeTool::from_config(&git2::Config::open(&p).unwrap()), MergeTool::Markers);
}

#[test]
fn log_stops_at_missing_parent() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("f", "1");
    let mut ea = a.create();
    for v in ["2", "3"] {
        a.write("f", v);
        ea.sync().unwrap();
    }
    let la = history::log(&ea, None, 100).unwrap();
    assert_eq!(la.len(), 3);
    assert!(la[0].time > la[1].time && la[1].time > la[2].time, "newest first");
    let (eb, _) = b.join(ea.project_id);
    let lb = history::log(&eb, None, 100).unwrap();
    assert_eq!(lb.len(), 1, "older snapshots are not on B");
    assert_eq!(lb[0].device_name, "a");
    assert_eq!(history::log(&ea, None, 2).unwrap().len(), 2);
    // path filter marks only snapshots that changed the path
    a.write("g", "x");
    ea.sync().unwrap();
    let lf = history::log(&ea, Some(&RelPath::new("f")), 100).unwrap();
    assert_eq!(lf.iter().map(|e| e.changed).collect::<Vec<_>>(), vec![false, true, true, true]);
}

#[test]
fn restore_before_time() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("f", "v1");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    a.write("f", "v2");
    ea.sync().unwrap();
    a.write("f", "v3");
    ea.sync().unwrap();
    let log = history::log(&ea, Some(&RelPath::new("f")), 10).unwrap();
    history::restore(&mut ea, &RelPath::new("f"), RestoreFrom::Before(log[1].time)).unwrap();
    assert_eq!(a.text("f"), "v2");
    history::restore(&mut ea, &RelPath::new("f"), RestoreFrom::Commit(log[2].commit)).unwrap();
    assert_eq!(a.text("f"), "v1");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed, "a restore is a normal change");
    eb.sync().unwrap();
    assert_eq!(b.text("f"), "v1");
    assert!(history::restore(&mut ea, &RelPath::new("f"), RestoreFrom::Before(0)).is_err());
    assert!(history::restore(&mut ea, &RelPath::new("f"), RestoreFrom::Join).is_err(), "no join snapshot on A");
    // restoring a path that did not exist yet removes it
    a.write("late", "x");
    ea.sync().unwrap();
    history::restore(&mut ea, &RelPath::new("late"), RestoreFrom::Commit(log[2].commit)).unwrap();
    assert!(a.read("late").is_none());
}

#[test]
fn restore_from_join() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write(".env", "DB=server\n");
    let ea = a.create();
    b.write(".env", "DB=local\n");
    let (mut eb, rep) = b.join(ea.project_id);
    assert_eq!(rep.differing, vec![RelPath::new(".env")]);
    assert_eq!(b.text(".env"), "DB=server\n");
    history::restore(&mut eb, &RelPath::new(".env"), RestoreFrom::Join).unwrap();
    assert_eq!(b.text(".env"), "DB=local\n");
}
