//! History, restore, merge-tool and watch-hook paths not reached by `resolve.rs` and `hooks.rs`.
#![cfg_attr(windows, allow(unused_imports, dead_code))] // helpers of #[cfg(unix)] tests

mod support;

use std::collections::HashSet;

use spoko_client::engine::{Engine, SyncOutcome};
use spoko_client::history::{self, RestoreFrom};
use spoko_client::hooks;
use spoko_client::resolve::MergeTool;
use spoko_core::platform::RelPath;
use support::*;

#[test]
fn log_of_a_project_never_synced_here_is_empty() {
    let mut w = World::new();
    let a = w.device("a");
    let c = w.device("c");
    a.write("x", "1");
    let ea = a.create();
    std::fs::create_dir_all(&c.root).unwrap();
    let ec = c.open(ea.project_id);
    assert!(history::log(&ec, None, 100).unwrap().is_empty());
}

#[test]
fn log_lists_a_common_ancestor_of_a_merge_once() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("one.txt", "1");
    a.write("two.txt", "2");
    let mut ea = a.create();
    let first = ea.state.base.unwrap();
    let (mut eb, _) = b.join(ea.project_id);
    a.write("one.txt", "1 from a");
    b.write("two.txt", "2 from b");
    ea.sync().unwrap();
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Merged { conflicts: 0 });
    let log = history::log(&eb, None, 100).unwrap();
    let commits: Vec<git2::Oid> = log.iter().map(|l| l.commit).collect();
    let unique: HashSet<git2::Oid> = commits.iter().copied().collect();
    assert_eq!(commits.len(), 4, "merge, both sides, and their base: {commits:?}");
    assert_eq!(unique.len(), commits.len());
    assert_eq!(commits.last(), Some(&first));
}

#[cfg(unix)]
#[test]
fn restore_brings_back_a_symlink() {
    let mut w = World::new();
    let a = w.device("a");
    a.write("target.txt", "t");
    std::fs::create_dir_all(&a.root).unwrap();
    std::os::unix::fs::symlink("target.txt", a.root.join("link")).unwrap();
    let mut ea = a.create();
    let with_link = ea.state.base.unwrap();
    std::fs::remove_file(a.root.join("link")).unwrap();
    a.write("link", "now a file");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
    history::restore(&mut ea, &RelPath::new("link"), RestoreFrom::Commit(with_link)).unwrap();
    assert_eq!(
        std::fs::read_link(a.root.join("link")).unwrap(),
        std::path::Path::new("target.txt")
    );
}

#[test]
fn restoring_an_absent_path_over_a_directory_fails() {
    let mut w = World::new();
    let a = w.device("a");
    a.write("x", "1");
    let mut ea = a.create();
    let before = ea.state.base.unwrap();
    a.write("d/inner.txt", "keep me");
    ea.sync().unwrap();
    // `d` did not exist in the first snapshot; restoring it would delete a directory
    assert!(history::restore(&mut ea, &RelPath::new("d"), RestoreFrom::Commit(before)).is_err());
    assert_eq!(a.text("d/inner.txt"), "keep me");
}

#[test]
fn merge_tool_outside_a_git_repository_comes_from_the_global_config() {
    let dir = tempfile::tempdir().unwrap();
    let global =
        git2::Config::open_default().map_or(MergeTool::Markers, |c| MergeTool::from_config(&c));
    assert_eq!(MergeTool::from_git_config(dir.path()), global);
}

#[test]
fn merge_tool_of_a_repository_comes_from_its_own_config() {
    let dir = tempfile::tempdir().unwrap();
    let repo = init_repo(dir.path());
    repo.config().unwrap().set_str("merge.tool", "mine").unwrap();
    repo.config().unwrap().set_str("mergetool.mine.cmd", "mine $MERGED").unwrap();
    assert_eq!(MergeTool::from_git_config(dir.path()), MergeTool::Command("mine $MERGED".into()));
}

#[test]
fn hook_for_a_missing_project_directory_fails_and_stays_pending() {
    let mut w = World::new();
    let a = w.device("a");
    a.write(
        ".spokoconfig",
        "[[watch]]\nfiles = [\"x.lock\"]\nrun = \"echo ok\"\nmode = \"auto\"\n",
    );
    let mut e: Engine = a.create();
    std::fs::remove_dir_all(&a.root).unwrap();
    hooks::on_remote_change(&mut e, vec![0]).unwrap();
    assert_eq!(e.state.pending_hooks, vec![0], "a hook that could not start stays queued");
    let runs = hooks::run_pending(&mut e).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, -1);
    assert!(!runs[0].output_tail.is_empty(), "the spawn error is reported");
    assert_eq!(e.state.pending_hooks, vec![0]);
    let log = std::fs::read_to_string(e.data_dir().join("hooks.log")).unwrap();
    assert!(log.contains("[exit -1]"), "{log}");
}
