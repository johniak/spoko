//! Engine paths the scenario tests in `sync.rs` do not reach: races during apply, server
//! faults (injected with SQLite triggers on the test server), git state merges, platform skips.
#![cfg_attr(windows, allow(unused_imports, dead_code))] // helpers of #[cfg(unix)] tests

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use spoko_client::ClientError;
use spoko_client::engine::{DeviceCtx, Engine, PauseReason, SyncOutcome};
use spoko_client::history::RestoreFrom;
use spoko_client::notifier::Notification;
use spoko_core::checkout::ApplyFaults;
use spoko_core::external::encode_claude_dir;
use spoko_core::platform::RelPath;
use spoko_core::snapshot::{read_snapshot, write_snapshot};
use spoko_core::store::{Entry, EntryMode, WorkMap};
use spoko_crypto::{name_id, seal};
use support::*;
use uuid::Uuid;

fn blob_oid(bytes: &[u8]) -> git2::Oid {
    git2::Oid::hash_object(git2::ObjectType::Blob, bytes).unwrap()
}

/// Claude Code chat directory of `d`'s project.
fn chat_dir(d: &Device) -> PathBuf {
    d.claude
        .join("projects")
        .join(encode_claude_dir(&d.root.to_string_lossy()))
}

/// Path of a loose object in an engine's internal store.
fn loose_object(e: &Engine, oid: git2::Oid) -> PathBuf {
    let hex = oid.to_string();
    e.store.repo().path().join("objects").join(&hex[..2]).join(&hex[2..])
}

/// Writes `rel` on another engine of the same device and pushes it (another spoko process).
fn push_from(ctx: DeviceCtx, project: Uuid, root: &Path, rel: &str, content: &str) {
    std::fs::write(root.join(rel), content).unwrap();
    let mut e = Engine::open(ctx, project, root).unwrap();
    assert_eq!(e.sync().unwrap(), SyncOutcome::Pushed);
}

fn has_branch(root: &Path, name: &str) -> bool {
    git2::Repository::open(root)
        .unwrap()
        .find_branch(name, git2::BranchType::Local)
        .is_ok()
}

fn branch_here(root: &Path, name: &str) {
    let repo = git2::Repository::open(root).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch(name, &head, false).unwrap();
}

struct FailAt(usize);
impl ApplyFaults for FailAt {
    fn before_write(&self, index: usize, _rel: &RelPath) -> std::io::Result<()> {
        if index >= self.0 {
            Err(std::io::Error::other("injected crash"))
        } else {
            Ok(())
        }
    }
}

// --- opening and creating ---

#[test]
fn create_in_a_missing_directory_fails_the_initial_push() {
    let mut w = World::new();
    let a = w.device("a");
    let Err(e) = Engine::create(a.ctx.clone(), &a.root, "proj") else {
        panic!("created a project without a directory");
    };
    assert_eq!(e.to_string(), "initial push failed: Paused");
    assert!(
        a.notes
            .take()
            .iter()
            .any(|n| matches!(n, Notification::MissingRoot { .. }))
    );
}

#[test]
fn unreadable_state_file_fails_open() {
    let mut w = World::new();
    let a = w.device("a");
    a.write("x", "1");
    let ea = a.create();
    let (id, state) = (ea.project_id, ea.data_dir().join("state.json"));
    drop(ea);
    std::fs::remove_file(&state).unwrap();
    std::fs::create_dir(&state).unwrap();
    let Err(e) = Engine::open(a.ctx.clone(), id, &a.root) else {
        panic!("opened with an unreadable state.json");
    };
    assert!(matches!(e, ClientError::Io(_)), "{e}");
}

#[test]
fn engine_time_comes_from_the_device_clock() {
    let mut w = World::new();
    let a = w.device("a");
    a.write("x", "1");
    let ea = a.create();
    let t = ea.now_public();
    assert_eq!(ea.now_public(), t + 1, "the test clock ticks once per reading");
}

// --- config files ---

#[test]
fn config_file_deleted_remotely_is_deleted_here() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write(".gitignore", "*.tmp\n");
    a.write("x", "1");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    assert_eq!(b.text(".gitignore"), "*.tmp\n");
    a.rm(".gitignore");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
    assert_eq!(eb.sync().unwrap(), SyncOutcome::FastForwarded);
    assert!(b.read(".gitignore").is_none());
    assert_eq!(b.text("x"), "1");
}

#[test]
fn config_edited_during_apply_is_merged_not_overwritten() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write(".gitignore", "*.a\n*.b\n*.c\n*.d\n*.e\n");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    a.write(".gitignore", "*.A\n*.b\n*.c\n*.d\n*.e\n");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
    let root = b.root.clone();
    eb.before_apply = Some(Box::new(move || {
        std::fs::write(root.join(".gitignore"), "*.a\n*.b\n*.c\n*.d\n*.e\n*.f\n").unwrap()
    }));
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Merged { conflicts: 0 });
    assert_eq!(b.text(".gitignore"), "*.A\n*.b\n*.c\n*.d\n*.e\n*.f\n");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::FastForwarded);
    assert_eq!(a.text(".gitignore"), "*.A\n*.b\n*.c\n*.d\n*.e\n*.f\n");
}

#[test]
fn failed_config_write_aborts_the_sync_until_retried() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write(".gitignore", "*.a\n");
    a.write("x", "1");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    a.write(".gitignore", "*.b\n");
    ea.sync().unwrap();
    let base = eb.state.base;
    eb.faults = Some(Box::new(FailAt(0)));
    let e = eb.sync().unwrap_err();
    assert!(e.to_string().contains("injected crash"), "{e}");
    assert_eq!(eb.state.base, base);
    assert_eq!(b.text(".gitignore"), "*.a\n");
    eb.faults = None;
    assert_eq!(eb.sync().unwrap(), SyncOutcome::FastForwarded);
    assert_eq!(b.text(".gitignore"), "*.b\n");
}

// --- paths that cannot be written here ---

/// A and B in sync; B then has an untracked directory `x/` where A adds a file `x`.
fn dir_in_the_way() -> (World, Device, Device, Engine, Engine) {
    let mut w = World::new();
    let a = w.device("a");
    let mut b = w.device("b");
    b.ctx.case_insensitive = Some(false);
    a.write(".gitignore", "*.log\n");
    a.write("seed.txt", "s");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    b.write("x/junk.log", "local only");
    a.write("x", "file from a");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
    assert_eq!(eb.sync().unwrap(), SyncOutcome::FastForwarded);
    (w, a, b, ea, eb)
}

#[test]
fn untracked_directory_in_the_way_is_skipped_and_not_deleted_remotely() {
    let (_w, a, b, mut ea, mut eb) = dir_in_the_way();
    let x = RelPath::new("x");
    assert!(b.root.join("x").is_dir());
    assert_eq!(b.text("x/junk.log"), "local only");
    assert_eq!(eb.state.unmaterialized[&x].oid, blob_oid(b"file from a"));
    let notes = b.notes.take();
    assert!(
        notes.iter().any(|n| matches!(n, Notification::PlatformSkip { path, reason, .. }
            if path == "x" && reason.contains("katalog"))),
        "{notes:?}"
    );
    // B's next push keeps `x`: A's file is not deleted
    b.write("other.txt", "from b");
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Pushed);
    assert_eq!(ea.sync().unwrap(), SyncOutcome::FastForwarded);
    assert_eq!(a.text("x"), "file from a");
    assert_eq!(a.text("other.txt"), "from b");
    // a newer version cannot be written either while the directory is there
    a.write("x", "v2");
    ea.sync().unwrap();
    assert_eq!(eb.sync().unwrap(), SyncOutcome::FastForwarded);
    assert!(b.root.join("x").is_dir());
    assert_eq!(eb.state.unmaterialized[&x].oid, blob_oid(b"v2"));
    // once the directory is gone, the next apply writes the file
    std::fs::remove_dir_all(b.root.join("x")).unwrap();
    a.write("seed.txt", "s2");
    ea.sync().unwrap();
    assert_eq!(eb.sync().unwrap(), SyncOutcome::FastForwarded);
    assert_eq!(b.text("x"), "v2");
    assert!(eb.state.unmaterialized.is_empty());
}

#[test]
fn remote_deletion_of_a_skipped_path_forgets_it() {
    let (_w, a, b, mut ea, mut eb) = dir_in_the_way();
    a.rm("x");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
    assert_eq!(eb.sync().unwrap(), SyncOutcome::FastForwarded);
    assert!(eb.state.unmaterialized.is_empty());
    assert_eq!(b.text("x/junk.log"), "local only", "the local directory is untouched");
    b.write("other.txt", "from b");
    eb.sync().unwrap();
    ea.sync().unwrap();
    assert!(a.read("x").is_none(), "B does not bring the deleted file back");
}

// --- externals ---

#[test]
fn chat_appended_during_apply_is_merged_with_the_remote_lines() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    std::fs::create_dir_all(chat_dir(&a)).unwrap();
    std::fs::write(chat_dir(&a).join("s1.jsonl"), "{\"uuid\":\"1\"}\n").unwrap();
    a.write("x", "1");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    let chat_b = chat_dir(&b).join("s1.jsonl");
    assert_eq!(std::fs::read_to_string(&chat_b).unwrap(), "{\"uuid\":\"1\"}\n");
    std::fs::write(chat_dir(&a).join("s1.jsonl"), "{\"uuid\":\"1\"}\n{\"uuid\":\"2\"}\n").unwrap();
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
    let path = chat_b.clone();
    eb.before_apply = Some(Box::new(move || {
        std::fs::write(path, "{\"uuid\":\"1\"}\n{\"uuid\":\"3\"}\n").unwrap()
    }));
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Merged { conflicts: 0 });
    let union = "{\"uuid\":\"1\"}\n{\"uuid\":\"3\"}\n{\"uuid\":\"2\"}\n";
    assert_eq!(std::fs::read_to_string(&chat_b).unwrap(), union);
    assert_eq!(ea.sync().unwrap(), SyncOutcome::FastForwarded);
    assert_eq!(std::fs::read_to_string(chat_dir(&a).join("s1.jsonl")).unwrap(), union);
}

#[test]
fn failed_chat_write_aborts_the_sync_until_retried() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    std::fs::create_dir_all(chat_dir(&a)).unwrap();
    std::fs::write(chat_dir(&a).join("s1.jsonl"), "{\"uuid\":\"1\"}\n").unwrap();
    a.write("x", "1");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    std::fs::write(chat_dir(&a).join("s1.jsonl"), "{\"uuid\":\"1\"}\n{\"uuid\":\"2\"}\n").unwrap();
    ea.sync().unwrap();
    eb.faults = Some(Box::new(FailAt(0)));
    let e = eb.sync().unwrap_err();
    assert!(e.to_string().contains("injected crash"), "{e}");
    let chat_b = chat_dir(&b).join("s1.jsonl");
    assert_eq!(std::fs::read_to_string(&chat_b).unwrap(), "{\"uuid\":\"1\"}\n");
    eb.faults = None;
    assert_eq!(eb.sync().unwrap(), SyncOutcome::FastForwarded);
    assert_eq!(
        std::fs::read_to_string(&chat_b).unwrap(),
        "{\"uuid\":\"1\"}\n{\"uuid\":\"2\"}\n"
    );
}

#[test]
fn custom_external_added_later_syncs_into_each_home() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("x", "1");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    let notes_a = a.ctx.env.home.join("notes");
    std::fs::create_dir_all(&notes_a).unwrap();
    std::fs::write(notes_a.join("todo.txt"), "buy milk\n").unwrap();
    a.write(
        ".spokoconfig",
        "[[external.custom]]\nname = \"notes\"\npath = \"{home}/notes\"\n",
    );
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
    assert_eq!(eb.sync().unwrap(), SyncOutcome::FastForwarded);
    let todo_b = b.ctx.env.home.join("notes/todo.txt");
    assert_eq!(std::fs::read_to_string(todo_b).unwrap(), "buy milk\n");
}

#[test]
fn merge_without_the_base_version_of_a_chat_file_fails() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    std::fs::create_dir_all(chat_dir(&a)).unwrap();
    std::fs::write(chat_dir(&a).join("memo.md"), "memo base\n").unwrap();
    a.write("x", "1");
    let mut ea = a.create();
    let (eb, _) = b.join(ea.project_id);
    let loose = loose_object(&eb, blob_oid(b"memo base\n"));
    drop(eb);
    std::fs::remove_file(&loose).expect("a small clone stores its objects loose");
    std::fs::write(chat_dir(&a).join("memo.md"), "memo from a\n").unwrap();
    ea.sync().unwrap();
    let memo_b = chat_dir(&b).join("memo.md");
    std::fs::write(&memo_b, "memo from b\n").unwrap();
    let mut eb = b.open(ea.project_id);
    let e = eb.sync().unwrap_err();
    assert!(e.to_string().contains("not found"), "{e}");
    assert_eq!(std::fs::read_to_string(&memo_b).unwrap(), "memo from b\n");
}

// --- merging ---

#[test]
fn merge_without_the_base_version_of_a_file_fails() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("f.txt", "base\n");
    let mut ea = a.create();
    let (eb, _) = b.join(ea.project_id);
    let loose = loose_object(&eb, blob_oid(b"base\n"));
    drop(eb);
    std::fs::remove_file(&loose).expect("a small clone stores its objects loose");
    a.write("f.txt", "from a\n");
    ea.sync().unwrap();
    b.write("f.txt", "from b\n");
    let mut eb = b.open(ea.project_id);
    let base = eb.state.base;
    let e = eb.sync().unwrap_err();
    assert!(e.to_string().contains("not found"), "{e}");
    assert_eq!(eb.state.base, base);
    assert_eq!(b.text("f.txt"), "from b\n");
}

#[test]
fn local_edit_during_merge_apply_is_merged_again() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("f.txt", "l1\nl2\nl3\nl4\nl5\n");
    a.write("g.txt", "g\n");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    a.write("f.txt", "L1\nl2\nl3\nl4\nl5\n");
    ea.sync().unwrap();
    b.write("g.txt", "g from b\n");
    let root = b.root.clone();
    eb.before_apply = Some(Box::new(move || {
        std::fs::write(root.join("f.txt"), "l1\nl2\nl3\nl4\nB5\n").unwrap()
    }));
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Merged { conflicts: 0 });
    assert_eq!(b.text("f.txt"), "L1\nl2\nl3\nl4\nB5\n");
    assert_eq!(b.text("g.txt"), "g from b\n");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::FastForwarded);
    assert_eq!(a.text("f.txt"), "L1\nl2\nl3\nl4\nB5\n");
}

/// The user saves `path` with `content` once, while spoko writes the first file of an apply.
struct SaveOnce(PathBuf, &'static str, std::sync::atomic::AtomicBool);
impl ApplyFaults for SaveOnce {
    fn before_write(&self, _index: usize, _rel: &RelPath) -> std::io::Result<()> {
        if !self.2.swap(true, Ordering::SeqCst) {
            std::fs::write(&self.0, self.1)?;
        }
        Ok(())
    }
}

#[test]
fn file_saved_while_the_apply_writes_others_is_merged_not_overwritten() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    for f in ["a.txt", "b.txt", "c.txt"] {
        a.write(f, "l1\nl2\nl3\n");
    }
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    for f in ["a.txt", "b.txt", "c.txt"] {
        a.write(f, "l1\nl2\nl3\nfrom a\n");
    }
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
    // B had no local change when the fast-forward started; the editor saves b.txt mid-way
    eb.faults = Some(Box::new(SaveOnce(b.root.join("b.txt"), "from b\nl1\nl2\nl3\n", false.into())));
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Merged { conflicts: 0 });
    for f in ["a.txt", "c.txt"] {
        assert_eq!(b.text(f), "l1\nl2\nl3\nfrom a\n");
    }
    assert_eq!(b.text("b.txt"), "from b\nl1\nl2\nl3\nfrom a\n");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::FastForwarded);
    assert_eq!(a.text("b.txt"), "from b\nl1\nl2\nl3\nfrom a\n");
}

#[test]
fn project_removed_during_apply_pauses() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("f.txt", "1");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    a.write("f.txt", "2");
    ea.sync().unwrap();
    let root = b.root.clone();
    eb.before_apply = Some(Box::new(move || std::fs::remove_dir_all(root).unwrap()));
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Paused);
    assert_eq!(eb.state.paused, Some(PauseReason::MissingRoot));
}

#[test]
fn project_removed_during_merge_apply_pauses() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("f.txt", "1");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    a.write("f.txt", "2");
    ea.sync().unwrap();
    b.write("g.txt", "from b");
    let root = b.root.clone();
    eb.before_apply = Some(Box::new(move || std::fs::remove_dir_all(root).unwrap()));
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Paused);
    assert_eq!(eb.state.paused, Some(PauseReason::MissingRoot));
    assert!(
        b.notes
            .take()
            .contains(&Notification::MissingRoot { project: eb.project_id })
    );
}

#[test]
fn merge_followed_by_a_newer_remote_fast_forwards_and_reports_the_merge() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("f.txt", "1");
    a.write("g.txt", "g");
    let mut ea = a.create();
    let id = ea.project_id;
    let (mut eb, _) = b.join(id);
    // the same edit on both machines: the merge result is A's snapshot itself
    a.write("f.txt", "same");
    b.write("f.txt", "same");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
    drop(ea);
    // while B applies it, A pushes again
    let (ctx, root) = (a.ctx.clone(), a.root.clone());
    eb.before_apply = Some(Box::new(move || push_from(ctx, id, &root, "g.txt", "g2")));
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Merged { conflicts: 0 });
    assert_eq!(b.text("f.txt"), "same");
    assert_eq!(b.text("g.txt"), "g2");
    assert_eq!(eb.sync().unwrap(), SyncOutcome::UpToDate);
}

#[test]
fn path_conflicting_again_keeps_only_its_newest_conflict() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("f.txt", "same\n");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    a.write("f.txt", "A1\n");
    b.write("f.txt", "B1\n");
    ea.sync().unwrap();
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Merged { conflicts: 1 });
    assert_eq!(ea.sync().unwrap(), SyncOutcome::FastForwarded);
    a.write("f.txt", "A2\n");
    b.write("f.txt", "B2\n");
    ea.sync().unwrap();
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Merged { conflicts: 1 });
    let conflicts = read_snapshot(&eb.store, eb.state.base.unwrap()).unwrap().meta.conflicts;
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    assert_eq!(conflicts[0].path, "work/f.txt");
    assert_eq!(conflicts[0].theirs, Some(blob_oid(b"A2\n")));
}

#[test]
fn null_notifier_still_reports_conflicts_through_the_outcome() {
    let mut w = World::new();
    let a = w.device("a");
    let mut b = w.device("b");
    b.ctx.notifier = std::sync::Arc::new(spoko_client::notifier::NullNotifier);
    a.write("f.txt", "same line\n");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    a.write("f.txt", "A version\n");
    b.write("f.txt", "B version\n");
    ea.sync().unwrap();
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Merged { conflicts: 1 });
    assert!(b.notes.take().is_empty(), "notifications went to the null notifier");
    assert_eq!(b.text("f.txt"), "B version\n");
}

// --- git state ---

/// A with a git repo (one commit) and B joined to it.
fn with_git() -> (World, Device, Device, Engine, Engine) {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    std::fs::create_dir_all(&a.root).unwrap();
    let repo = init_repo(&a.root);
    a.write("f.txt", "1\n");
    a.write("g.txt", "2\n");
    commit_all(&repo, "init");
    let ea = a.create();
    let (eb, _) = b.join(ea.project_id);
    (w, a, b, ea, eb)
}

#[test]
fn concurrent_file_edits_with_the_same_git_state_merge_cleanly() {
    let (_w, a, b, mut ea, mut eb) = with_git();
    let gitstate = read_snapshot(&eb.store, eb.state.base.unwrap()).unwrap().gitstate;
    a.write("f.txt", "1 from a\n");
    b.write("g.txt", "2 from b\n");
    ea.sync().unwrap();
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Merged { conflicts: 0 });
    let merged = read_snapshot(&eb.store, eb.state.base.unwrap()).unwrap();
    assert_eq!(merged.gitstate, gitstate);
    assert_eq!(b.text("f.txt"), "1 from a\n");
    assert_eq!(b.text("g.txt"), "2 from b\n");
}

#[test]
fn branch_created_during_apply_is_merged_with_the_remote_git_state() {
    let (_w, a, b, mut ea, mut eb) = with_git();
    branch_here(&a.root, "from-a");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
    let root = b.root.clone();
    eb.before_apply = Some(Box::new(move || branch_here(&root, "from-b")));
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Merged { conflicts: 0 });
    assert_eq!(ea.sync().unwrap(), SyncOutcome::FastForwarded);
    for root in [&a.root, &b.root] {
        assert!(has_branch(root, "from-a") && has_branch(root, "from-b"));
    }
}

#[test]
fn older_local_branch_merges_with_a_newer_remote_branch() {
    let (_w, a, b, mut ea, mut eb) = with_git();
    branch_here(&b.root, "from-b");
    eb.capture(None).unwrap(); // B's change is the older one
    branch_here(&a.root, "from-a");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Merged { conflicts: 0 });
    assert_eq!(ea.sync().unwrap(), SyncOutcome::FastForwarded);
    for root in [&a.root, &b.root] {
        assert!(has_branch(root, "from-a") && has_branch(root, "from-b"));
    }
}

#[test]
fn deferred_git_state_merges_with_local_git_changes() {
    let (_w, a, b, mut ea, mut eb) = with_git();
    let repo = git2::Repository::open(&a.root).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch("dzban", &head, false).unwrap();
    repo.set_head("refs/heads/dzban").unwrap();
    a.write("f.txt", "changed\n");
    ea.sync().unwrap();
    // git starts on B right after the files were written
    eb.before_git = Some(lock_git_on_call(&b.root, 1));
    assert_eq!(eb.sync().unwrap(), SyncOutcome::FastForwarded);
    assert!(eb.state.pending_gitstate.is_some());
    assert!(
        b.notes
            .take()
            .contains(&Notification::GitBusy { project: eb.project_id })
    );
    // git is free again, and B made a branch of its own meanwhile
    eb.before_git = None;
    std::fs::remove_file(b.root.join(".git/index.lock")).unwrap();
    branch_here(&b.root, "mine");
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Pushed);
    assert_eq!(eb.state.pending_gitstate, None);
    let rb = git2::Repository::open(&b.root).unwrap();
    assert_eq!(rb.head().unwrap().name().unwrap(), "refs/heads/dzban");
    assert!(has_branch(&b.root, "mine"));
    assert_eq!(ea.sync().unwrap(), SyncOutcome::FastForwarded);
    assert!(has_branch(&a.root, "mine"));
    assert_eq!(repo.head().unwrap().name().unwrap(), "refs/heads/dzban");
}

#[test]
fn git_repository_made_on_one_machine_reaches_one_without_git() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("one.txt", "1\n");
    a.write("two.txt", "2\n");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    let rb = init_repo(&b.root);
    let c = commit_all(&rb, "init on b");
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Pushed);
    a.write("one.txt", "1 from a\n");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Merged { conflicts: 0 });
    let ra = git2::Repository::open(&a.root).unwrap();
    assert_eq!(ra.head().unwrap().target(), Some(c));
    assert_eq!(a.text("one.txt"), "1 from a\n");
}

#[test]
fn git_state_in_an_unknown_format_is_kept_from_the_newer_side() {
    let (_w, a, b, mut ea, mut eb) = with_git();
    branch_here(&a.root, "from-a");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
    // B's last snapshot carries a gitstate section this version cannot read (e.g. written by
    // a newer spoko); a merge is in progress on B, so B's capture keeps that section as it is
    std::fs::write(b.root.join(".git/MERGE_HEAD"), "").unwrap();
    let last = eb.state.last_local.unwrap();
    let mut parts = read_snapshot(&eb.store, last).unwrap();
    let mut unknown = WorkMap::default();
    let oid = eb.store.write_blob(b"v9").unwrap();
    unknown.insert(RelPath::new("state-v9.bin"), Entry { oid, mode: EntryMode::Blob });
    let unknown = eb.store.build_tree(&unknown).unwrap();
    parts.gitstate = Some(unknown);
    parts.meta.time = eb.now_public();
    eb.state.last_local = Some(write_snapshot(&eb.store, &parts, &[last]).unwrap());
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Merged { conflicts: 0 });
    let merged = read_snapshot(&eb.store, eb.state.base.unwrap()).unwrap();
    assert_eq!(merged.gitstate, Some(unknown));
    // A cannot read it either and leaves its repository alone
    assert_eq!(ea.sync().unwrap(), SyncOutcome::FastForwarded);
    assert!(has_branch(&a.root, "from-a"));
}

/// `before_git` hook that takes git's index lock on its `n`-th call (1-based): another git
/// process starting right after spoko checked that git is free.
fn lock_git_on_call(root: &Path, n: usize) -> Box<dyn Fn() + Send> {
    let (lock, calls) = (root.join(".git/index.lock"), AtomicUsize::new(0));
    Box::new(move || {
        if calls.fetch_add(1, Ordering::SeqCst) + 1 == n {
            std::fs::write(&lock, "").unwrap();
        }
    })
}

/// A switched to a new branch and pushed.
fn switch_on_a(a: &Device, ea: &mut Engine) {
    let repo = git2::Repository::open(&a.root).unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch("dzban", &head, false).unwrap();
    repo.set_head("refs/heads/dzban").unwrap();
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
}

fn head_name(root: &Path) -> String {
    let repo = git2::Repository::open(root).unwrap();
    repo.head().unwrap().name().unwrap().to_owned()
}

#[test]
fn git_taken_busy_right_before_the_git_apply_defers_it() {
    let (_w, a, b, mut ea, mut eb) = with_git();
    switch_on_a(&a, &mut ea);
    eb.before_git = Some(lock_git_on_call(&b.root, 1));
    assert_eq!(eb.sync().unwrap(), SyncOutcome::FastForwarded);
    assert!(eb.state.pending_gitstate.is_some());
    assert!(
        b.notes
            .take()
            .contains(&Notification::GitBusy { project: eb.project_id })
    );
    assert_ne!(head_name(&b.root), "refs/heads/dzban");
    eb.before_git = None;
    std::fs::remove_file(b.root.join(".git/index.lock")).unwrap();
    assert_eq!(eb.sync().unwrap(), SyncOutcome::UpToDate);
    assert_eq!(head_name(&b.root), "refs/heads/dzban");
    assert_eq!(eb.state.pending_gitstate, None);
}

#[test]
fn git_taken_busy_before_a_deferred_apply_does_not_revert_the_remote_git_state() {
    let (_w, a, b, mut ea, mut eb) = with_git();
    switch_on_a(&a, &mut ea);
    let lock = b.root.join(".git/index.lock");
    eb.before_git = Some(lock_git_on_call(&b.root, 1));
    assert_eq!(eb.sync().unwrap(), SyncOutcome::FastForwarded);
    assert!(eb.state.pending_gitstate.is_some());
    // git is free when B's capture checks it, and busy again when it applies the deferred state
    std::fs::remove_file(&lock).unwrap();
    eb.before_git = Some(lock_git_on_call(&b.root, 1));
    assert_eq!(eb.sync().unwrap(), SyncOutcome::UpToDate);
    assert!(eb.state.pending_gitstate.is_some());
    assert_eq!(ea.sync().unwrap(), SyncOutcome::UpToDate);
    assert_eq!(head_name(&a.root), "refs/heads/dzban", "B pushed its stale git state");
    eb.before_git = None;
    std::fs::remove_file(&lock).unwrap();
    assert_eq!(eb.sync().unwrap(), SyncOutcome::UpToDate);
    assert_eq!(head_name(&b.root), "refs/heads/dzban");
}

#[test]
fn git_taken_busy_right_after_the_git_apply_is_recaptured_later() {
    let (_w, a, b, mut ea, mut eb) = with_git();
    switch_on_a(&a, &mut ea);
    eb.before_git = Some(lock_git_on_call(&b.root, 2));
    assert_eq!(eb.sync().unwrap(), SyncOutcome::FastForwarded);
    assert_eq!(head_name(&b.root), "refs/heads/dzban");
    assert_eq!(eb.state.pending_gitstate, None);
    assert_eq!(eb.state.applied_gitstate, None, "unknown until git is free again");
    eb.before_git = None;
    std::fs::remove_file(b.root.join(".git/index.lock")).unwrap();
    assert_eq!(eb.sync().unwrap(), SyncOutcome::UpToDate);
    assert!(eb.state.applied_gitstate.is_some());
    assert_eq!(ea.sync().unwrap(), SyncOutcome::UpToDate);
}

// --- server faults ---

#[test]
fn malformed_head_pointer_is_an_integrity_error() {
    let mut w = World::new();
    let a = w.device("a");
    a.write("x", "1");
    let mut ea = a.create();
    let head = name_id(&a.ctx.keyring, "ptr", "head");
    let short = seal(&a.ctx.keyring, b"head", b"short");
    w.srv
        .state
        .db
        .cas_pointer(ea.project_id, &head, None, &short)
        .unwrap()
        .unwrap();
    let base = ea.state.base;
    let e = ea.sync().unwrap_err();
    assert!(matches!(e, ClientError::Integrity(ref m) if m == "bad head pointer"), "{e}");
    assert_eq!(ea.state.base, base);
}

#[test]
fn head_moved_between_read_and_swap_is_retried() {
    let mut w = World::new();
    let a = w.device("a");
    a.write("x", "1");
    let mut ea = a.create();
    let before = ea.state.head_version;
    // the second request (the upload, between reading and swapping the head) moves the head
    w.srv
        .state
        .db
        .execute_raw(
            "CREATE TABLE fuse (n INTEGER NOT NULL); INSERT INTO fuse VALUES (2);
             CREATE TRIGGER move_head AFTER UPDATE ON devices WHEN (SELECT n FROM fuse) > 0
             BEGIN
                 UPDATE fuse SET n = n - 1;
                 UPDATE pointers SET version = version + 1 WHERE (SELECT n FROM fuse) = 0;
             END;",
        )
        .unwrap();
    a.write("x", "2");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
    assert_eq!(ea.state.head_version, before + 2, "lost one swap, won the retry");
    let b = w.device("b");
    let (_eb, _) = b.join(ea.project_id);
    assert_eq!(b.text("x"), "2");
}

#[test]
fn head_moving_on_every_request_gives_up_after_ten_rounds() {
    let mut w = World::new();
    let a = w.device("a");
    a.write("x", "1");
    let mut ea = a.create();
    let db = &w.srv.state.db;
    db.execute_raw(
        "CREATE TRIGGER move_head AFTER UPDATE ON devices
         BEGIN UPDATE pointers SET version = version + 1; END;",
    )
    .unwrap();
    a.write("x", "2");
    let base = ea.state.base;
    let e = ea.sync().unwrap_err();
    assert_eq!(e.to_string(), "sync did not converge after 10 rounds");
    assert_eq!(ea.state.base, base);
    db.execute_raw("DROP TRIGGER move_head;").unwrap();
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
}

#[test]
fn server_error_on_head_update_fails_the_sync() {
    let mut w = World::new();
    let a = w.device("a");
    a.write("x", "1");
    let mut ea = a.create();
    let db = &w.srv.state.db;
    db.execute_raw(
        "CREATE TRIGGER no_pointers BEFORE UPDATE ON pointers
         BEGIN SELECT RAISE(ABORT, 'pointer storage down'); END;",
    )
    .unwrap();
    a.write("x", "2");
    let base = ea.state.base;
    let e = ea.sync().unwrap_err();
    assert!(matches!(e, ClientError::Server(_)), "{e}");
    assert_eq!(ea.state.base, base);
    db.execute_raw("DROP TRIGGER no_pointers;").unwrap();
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
}

#[test]
fn server_failing_after_the_head_read_fails_the_upload() {
    let mut w = World::new();
    let a = w.device("a");
    a.write("x", "1");
    let mut ea = a.create();
    let db = &w.srv.state.db;
    // every request after the first fails (the aborted countdown is rolled back)
    db.execute_raw(
        "CREATE TABLE fuse (n INTEGER NOT NULL); INSERT INTO fuse VALUES (2);
         CREATE TRIGGER outage BEFORE UPDATE ON devices WHEN (SELECT n FROM fuse) > 0
         BEGIN
             UPDATE fuse SET n = n - 1;
             SELECT RAISE(ABORT, 'outage') WHERE (SELECT n FROM fuse) = 0;
         END;",
    )
    .unwrap();
    a.write("x", "2");
    let (base, version) = (ea.state.base, ea.state.head_version);
    let e = ea.sync().unwrap_err();
    assert!(matches!(e, ClientError::Server(_)), "{e}");
    assert_eq!((ea.state.base, ea.state.head_version), (base, version));
    db.execute_raw("DROP TRIGGER outage;").unwrap();
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
}

#[test]
fn activity_pointer_failure_does_not_fail_the_push() {
    let mut w = World::new();
    let a = w.device("a");
    let activity = name_id(&a.ctx.keyring, "ptr", "activity");
    let db = &w.srv.state.db;
    db.execute_raw(&format!(
        "CREATE TRIGGER no_activity_insert BEFORE INSERT ON pointers WHEN NEW.name = '{activity}'
         BEGIN SELECT RAISE(ABORT, 'activity down'); END;
         CREATE TRIGGER no_activity_update BEFORE UPDATE ON pointers WHEN NEW.name = '{activity}'
         BEGIN SELECT RAISE(ABORT, 'activity down'); END;"
    ))
    .unwrap();
    a.write("x", "1");
    let mut ea = a.create();
    assert_eq!(ea.last_activity().unwrap(), None);
    assert_eq!(ea.state.last_activity_push, 0);
    db.execute_raw("DROP TRIGGER no_activity_insert; DROP TRIGGER no_activity_update;")
        .unwrap();
    a.write("x", "2");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
    let (device, time, version) = ea.last_activity().unwrap().unwrap();
    assert_eq!(device, a.ctx.device_id);
    assert_eq!(time, ea.state.last_activity_push);
    assert_eq!(version, 1);
    // a value that is not an activity record is an integrity error
    let junk = seal(&a.ctx.keyring, b"activity", b"not json");
    db.cas_pointer(ea.project_id, &activity, None, &junk)
        .unwrap()
        .unwrap();
    assert!(matches!(ea.last_activity(), Err(ClientError::Integrity(_))));
}

// --- disk faults ---

#[cfg(unix)]
#[test]
fn dirty_path_under_a_symlink_loop_fails_the_sync() {
    let mut w = World::new();
    let a = w.device("a");
    a.write("x", "1");
    let mut ea = a.create();
    std::os::unix::fs::symlink("loop", a.root.join("loop")).unwrap();
    let dirty = [RelPath::new("loop/x")].into();
    let e = ea.sync_dirty(Some(&dirty)).unwrap_err();
    assert!(e.to_string().contains("symbolic links"), "{e}");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed, "a full scan records the link itself");
}

/// Makes `dir` unlistable; `false` when permissions are not enforced (running as root).
#[cfg(unix)]
fn make_unlistable(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o300)).unwrap();
    if std::fs::read_dir(dir).is_ok() {
        make_listable(dir);
        return false;
    }
    true
}

#[cfg(unix)]
fn make_listable(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn unlistable_project_directory_fails_the_sync() {
    let mut w = World::new();
    let a = w.device("a");
    a.write("x", "1");
    let mut ea = a.create();
    if !make_unlistable(&a.root) {
        return;
    }
    let r = ea.sync();
    make_listable(&a.root);
    let e = r.unwrap_err();
    assert!(e.to_string().contains("ermission denied"), "{e}");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::UpToDate);
}

#[cfg(unix)]
#[test]
fn unlistable_chat_directory_fails_the_sync() {
    let mut w = World::new();
    let a = w.device("a");
    std::fs::create_dir_all(chat_dir(&a)).unwrap();
    std::fs::write(chat_dir(&a).join("s1.jsonl"), "{\"uuid\":\"1\"}\n").unwrap();
    a.write("x", "1");
    let mut ea = a.create();
    if !make_unlistable(&chat_dir(&a)) {
        return;
    }
    let r = ea.sync();
    make_listable(&chat_dir(&a));
    let e = r.unwrap_err();
    assert!(e.to_string().contains("ermission denied"), "{e}");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::UpToDate);
}

// --- joining ---

#[test]
fn joining_a_project_without_a_snapshot_fails() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    // registered on the server, but its first push never happened
    let meta = seal(&a.ctx.keyring, b"project", b"proj");
    let info = a.ctx.api.create_project(&meta).unwrap();
    let Err(e) = Engine::clone_into(b.ctx.clone(), info.id, &b.root) else {
        panic!("joined a project with nothing in it");
    };
    assert_eq!(e.to_string(), "project has no snapshot yet");
}

#[test]
fn join_while_git_runs_in_the_clone_asks_to_run_again() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("f.txt", "project\n");
    let ea = a.create();
    std::fs::create_dir_all(&b.root).unwrap();
    init_repo(&b.root);
    b.write("f.txt", "mine\n");
    std::fs::write(b.root.join(".git/index.lock"), "").unwrap();
    let Err(e) = Engine::clone_into(b.ctx.clone(), ea.project_id, &b.root) else {
        panic!("joined while git was writing the work tree");
    };
    assert_eq!(e.to_string(), "git is running in the project directory; run it again when it is done");
    assert_eq!(b.text("f.txt"), "mine\n");
    std::fs::remove_file(b.root.join(".git/index.lock")).unwrap();
    let (_eb, rep) = b.join(ea.project_id);
    assert_eq!(b.text("f.txt"), "project\n");
    assert!(rep.differing.contains(&RelPath::new("f.txt")), "{:?}", rep.differing);
}

#[cfg(unix)]
#[test]
fn join_over_an_ignored_symlink_asks_to_run_again() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("link", "a regular file\n");
    let ea = a.create();
    std::fs::create_dir_all(&b.root).unwrap();
    b.write(".gitignore", "link\n");
    std::os::unix::fs::symlink("elsewhere", b.root.join("link")).unwrap();
    let Err(e) = Engine::clone_into(b.ctx.clone(), ea.project_id, &b.root) else {
        panic!("join replaced a symlink it never captured");
    };
    assert_eq!(e.to_string(), "files changed during join; run it again");
    assert_eq!(std::fs::read_link(b.root.join("link")).unwrap(), Path::new("elsewhere"));
    // the project has no .gitignore, so the second run captures the link and replaces it
    let (_eb, rep) = b.join(ea.project_id);
    assert_eq!(b.text("link"), "a regular file\n");
    assert!(rep.differing.contains(&RelPath::new("link")), "{:?}", rep.differing);
}

#[cfg(unix)]
#[test]
fn files_replaced_by_an_unfinished_join_stay_restorable() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("link", "a regular file\n");
    let ea = a.create();
    std::fs::create_dir_all(&b.root).unwrap();
    b.write(".gitignore", "link\n");
    std::os::unix::fs::symlink("elsewhere", b.root.join("link")).unwrap();
    assert!(Engine::clone_into(b.ctx.clone(), ea.project_id, &b.root).is_err());
    assert!(b.read(".gitignore").is_none(), "the first run already applied the project's config");
    let (mut eb, _) = b.join(ea.project_id);
    let gitignore = RelPath::new(".gitignore");
    spoko_client::history::restore(&mut eb, &gitignore, RestoreFrom::Join).unwrap();
    assert_eq!(b.text(".gitignore"), "link\n");
}
