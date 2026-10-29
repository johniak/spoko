mod support;

use std::collections::BTreeSet;

use spoko_client::engine::{PauseReason, SyncOutcome};
use spoko_client::notifier::Notification;
use spoko_core::checkout::ApplyFaults;
use spoko_core::platform::RelPath;
use spoko_core::snapshot::read_snapshot;
use support::*;

#[test]
fn edit_on_a_appears_on_b() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("src/main.rs", "fn main() {}\n");
    let mut ea = a.create();
    let (mut eb, rep) = b.join(ea.project_id);
    assert!(rep.differing.is_empty());
    assert_eq!(b.text("src/main.rs"), "fn main() {}\n");
    a.write("src/lib.rs", "pub fn x() {}\n");
    a.write("bin/run.sh", "#!/bin/sh\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            a.root.join("bin/run.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
    assert_eq!(eb.sync().unwrap(), SyncOutcome::FastForwarded);
    assert_eq!(tree(&a.root), tree(&b.root));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(b.root.join("bin/run.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0o111
        );
    }
    assert_eq!(eb.sync().unwrap(), SyncOutcome::UpToDate);
    assert_eq!(ea.sync().unwrap(), SyncOutcome::UpToDate);
    // deletion propagates
    a.rm("src/lib.rs");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
    eb.sync().unwrap();
    assert!(b.read("src/lib.rs").is_none());
}

#[test]
fn incremental_dirty_capture() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("x.txt", "1");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    a.write("x.txt", "2");
    a.write("y.txt", "not dirty yet");
    let dirty: BTreeSet<RelPath> = [RelPath::new("x.txt")].into();
    assert_eq!(ea.sync_dirty(Some(&dirty)).unwrap(), SyncOutcome::Pushed);
    eb.sync().unwrap();
    assert_eq!(b.text("x.txt"), "2");
    assert!(b.read("y.txt").is_none());
}

#[test]
fn spec_git_example() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    std::fs::create_dir_all(&a.root).unwrap();
    let repo = init_repo(&a.root);
    a.write("README.md", "hello\n");
    commit_all(&repo, "init");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    // johniak: commit, then switch to dzban
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch("johniak", &head, false).unwrap();
    repo.set_head("refs/heads/johniak").unwrap();
    a.write("feature.rs", "// johniak work\n");
    let c = commit_all(&repo, "johniak work");
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch("dzban", &head, false).unwrap();
    repo.set_head("refs/heads/dzban").unwrap();
    ea.sync().unwrap();
    eb.sync().unwrap();
    let rb = git2::Repository::open(&b.root).unwrap();
    assert_eq!(rb.head().unwrap().name().unwrap(), "refs/heads/dzban");
    assert_eq!(
        rb.find_reference("refs/heads/johniak").unwrap().target(),
        Some(c)
    );
    assert_eq!(
        rb.find_commit(c).unwrap().message().unwrap(),
        "johniak work"
    );
    let st = rb
        .statuses(Some(git2::StatusOptions::new().include_untracked(true)))
        .unwrap();
    let dirty: Vec<String> = st.iter().map(|s| s.path().unwrap().to_owned()).collect();
    assert!(dirty.is_empty(), "git status not clean on B: {dirty:?}");
    // and nothing ping-pongs back
    assert_eq!(eb.sync().unwrap(), SyncOutcome::UpToDate);
    assert_eq!(ea.sync().unwrap(), SyncOutcome::UpToDate);
}

#[test]
fn env_included_node_modules_not() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write(".gitignore", ".env\nnode_modules/\n");
    a.write(".spokoconfig", "[ignore]\ninclude = [\".env*\"]\n");
    a.write(".env", "SECRET=1\n");
    a.write(".env.local", "LOCAL=1\n");
    a.write("node_modules/react/index.js", "x");
    a.write("package-lock.json", "{}");
    let ea = a.create();
    let (_eb, _) = b.join(ea.project_id);
    assert_eq!(b.text(".env"), "SECRET=1\n");
    assert_eq!(b.text(".env.local"), "LOCAL=1\n");
    assert!(b.read("node_modules/react/index.js").is_none());
    assert_eq!(b.text("package-lock.json"), "{}");
}

#[test]
fn concurrent_edits_different_files_merge_cleanly() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("one.txt", "1\n");
    a.write("two.txt", "2\n");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    a.write("one.txt", "1 from a\n");
    b.write("two.txt", "2 from b\n");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Merged { conflicts: 0 });
    assert_eq!(ea.sync().unwrap(), SyncOutcome::FastForwarded);
    for d in [&a, &b] {
        assert_eq!(d.text("one.txt"), "1 from a\n");
        assert_eq!(d.text("two.txt"), "2 from b\n");
    }
    assert_eq!(eb.sync().unwrap(), SyncOutcome::UpToDate);
}

#[test]
fn concurrent_edits_same_file_clean_merge() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("f.txt", "l1\nl2\nl3\nl4\nl5\n");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    a.write("f.txt", "L1\nl2\nl3\nl4\nl5\n");
    b.write("f.txt", "l1\nl2\nl3\nl4\nL5\n");
    ea.sync().unwrap();
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Merged { conflicts: 0 });
    ea.sync().unwrap();
    assert_eq!(a.text("f.txt"), "L1\nl2\nl3\nl4\nL5\n");
    assert_eq!(b.text("f.txt"), "L1\nl2\nl3\nl4\nL5\n");
}

#[test]
fn conflict_keeps_active_version_everywhere() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("f.txt", "same line\n");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    a.write("f.txt", "A version\n");
    b.write("f.txt", "B version\n");
    ea.sync().unwrap();
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Merged { conflicts: 1 });
    assert_eq!(b.text("f.txt"), "B version\n");
    ea.sync().unwrap();
    assert_eq!(a.text("f.txt"), "B version\n");
    let head = ea.state.base.unwrap();
    let meta = read_snapshot(&ea.store, head).unwrap().meta;
    assert_eq!(meta.conflicts.len(), 1);
    let c = &meta.conflicts[0];
    assert_eq!(c.path, "work/f.txt");
    assert_eq!(
        c.theirs,
        Some(git2::Oid::hash_object(git2::ObjectType::Blob, b"A version\n").unwrap())
    );
    assert_eq!(c.theirs_device, "a");
    assert!(
        b.notes
            .take()
            .iter()
            .any(|n| matches!(n, Notification::Conflict { path, .. } if path == "work/f.txt"))
    );
    // the conflict survives later syncs until resolved
    a.write("other.txt", "x");
    ea.sync().unwrap();
    eb.sync().unwrap();
    assert_eq!(
        read_snapshot(&eb.store, eb.state.base.unwrap())
            .unwrap()
            .meta
            .conflicts
            .len(),
        1
    );
}

#[test]
fn binary_newer_wins_and_history_keeps_older() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("img.png", b"\x89PNG\0base");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    a.write("img.png", b"\x89PNG\0older-from-a");
    b.write("img.png", b"\x89PNG\0newer-from-b");
    ea.sync().unwrap(); // A captures first → older
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Merged { conflicts: 0 });
    ea.sync().unwrap();
    assert_eq!(a.read("img.png").unwrap(), b"\x89PNG\0newer-from-b");
    // the older version is still reachable from the merge snapshot's parents
    let parents = eb
        .store
        .repo()
        .find_commit(eb.state.base.unwrap())
        .unwrap()
        .parent_ids()
        .count();
    let older = git2::Oid::hash_object(git2::ObjectType::Blob, b"\x89PNG\0older-from-a").unwrap();
    assert_eq!(parents, 2);
    assert!(eb.store.has(older));
    // a merge where B is older: A's binary is newer
    a.write("img.png", b"\x89PNG\0a2");
    b.write("img.png", b"\x89PNG\0b2");
    // B captures first this time
    let _ = eb.capture(None).unwrap();
    ea.sync().unwrap();
    eb.sync().unwrap();
    assert_eq!(b.read("img.png").unwrap(), b"\x89PNG\0a2");
    assert!(b.notes.take().iter().any(
        |n| matches!(n, Notification::BinaryOverwritten { path, .. } if path == "work/img.png")
    ));
}

#[test]
fn cas_race_three_devices_converge() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    let c = w.device("c");
    a.write("base.txt", "b");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    let (mut ec, _) = c.join(ea.project_id);
    a.write("a.txt", "a");
    b.write("b.txt", "b");
    c.write("c.txt", "c");
    // capture everywhere first, so all three race against the same base
    ea.capture(None).unwrap();
    eb.capture(None).unwrap();
    ec.capture(None).unwrap();
    for _ in 0..2 {
        ea.sync().unwrap();
        eb.sync().unwrap();
        ec.sync().unwrap();
    }
    assert_eq!(tree(&a.root), tree(&b.root));
    assert_eq!(tree(&b.root), tree(&c.root));
    assert_eq!(tree(&a.root).len(), 4);
}

#[test]
fn local_edit_during_merge_is_not_lost() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("f.txt", "l1\nl2\nl3\nl4\nl5\n");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    a.write("f.txt", "L1\nl2\nl3\nl4\nl5\n");
    ea.sync().unwrap();
    let root = b.root.clone();
    eb.before_apply = Some(Box::new(move || {
        std::fs::write(root.join("f.txt"), "l1\nl2\nl3\nl4\nB5\n").unwrap()
    }));
    eb.sync().unwrap();
    assert_eq!(b.text("f.txt"), "L1\nl2\nl3\nl4\nB5\n");
    ea.sync().unwrap();
    assert_eq!(a.text("f.txt"), "L1\nl2\nl3\nl4\nB5\n");
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

#[test]
fn crash_mid_apply_converges() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write("seed", "s");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    for i in 0..20 {
        a.write(&format!("f{i:02}.txt"), format!("{i}"));
    }
    ea.sync().unwrap();
    let base_before = eb.state.base;
    eb.faults = Some(Box::new(FailAt(10)));
    assert!(eb.sync().is_err());
    assert_eq!(eb.state.base, base_before);
    drop(eb);
    std::fs::write(b.root.join(".spoko-tmp-leftover"), "x").unwrap();
    let mut eb = b.open(ea.project_id);
    assert!(b.read(".spoko-tmp-leftover").is_none());
    eb.sync().unwrap();
    assert_eq!(tree(&a.root), tree(&b.root));
    assert_eq!(eb.sync().unwrap(), SyncOutcome::UpToDate);
    ea.sync().unwrap();
    assert_eq!(tree(&a.root), tree(&b.root));
}

#[test]
fn crash_during_first_download_converges() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    for i in 0..30 {
        a.write(&format!("d{}/f{i}", i % 3), format!("{i}"));
    }
    let ea = a.create();
    std::fs::create_dir_all(&b.root).unwrap();
    // B's first clone dies mid-apply; re-running clone/open converges
    let mut eb = b.open(ea.project_id);
    eb.faults = Some(Box::new(FailAt(5)));
    assert!(eb.sync().is_err());
    drop(eb);
    let mut eb = b.open(ea.project_id);
    eb.sync().unwrap();
    assert_eq!(tree(&a.root), tree(&b.root));
}

#[test]
fn mass_delete_pauses_project() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    for i in 0..100 {
        a.write(&format!("f{i:03}"), format!("{i}"));
    }
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    for i in 0..60 {
        a.rm(&format!("f{i:03}"));
    }
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Paused);
    assert_eq!(
        ea.state.paused,
        Some(PauseReason::MassDelete {
            deleted: 60,
            total: 100
        })
    );
    assert!(a.notes.take().contains(&Notification::MassDelete {
        project: ea.project_id,
        deleted: 60,
        total: 100
    }));
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Paused);
    eb.sync().unwrap();
    assert_eq!(tree(&b.root).len(), 100, "nothing was pushed");
    assert!(ea.resume(false).is_err());
    ea.resume(true).unwrap();
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed);
    eb.sync().unwrap();
    assert_eq!(tree(&b.root).len(), 40);
}

#[test]
fn missing_root_pauses_project() {
    let mut w = World::new();
    let a = w.device("a");
    a.write("x", "1");
    let mut ea = a.create();
    std::fs::rename(&a.root, a.root.with_extension("moved")).unwrap();
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Paused);
    assert_eq!(ea.state.paused, Some(PauseReason::MissingRoot));
    assert!(ea.resume(false).is_err());
    std::fs::rename(a.root.with_extension("moved"), &a.root).unwrap();
    ea.resume(false).unwrap();
    assert_eq!(ea.sync().unwrap(), SyncOutcome::UpToDate);
}

#[test]
fn join_existing_clone_keeps_local_diffs_in_join_snapshot() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    std::fs::create_dir_all(&a.root).unwrap();
    let ra = init_repo(&a.root);
    a.write(".gitignore", ".env\n");
    a.write(".spokoconfig", "[ignore]\ninclude = [\".env\"]\n");
    a.write("app.rs", "fn app() {}\n");
    commit_all(&ra, "init");
    a.write(".env", "DB=server\n");
    let ea = a.create();
    // B: an existing clone of the same repo with its own .env and a local branch
    std::fs::create_dir_all(&b.root).unwrap();
    let rb = init_repo(&b.root);
    b.write(".gitignore", ".env\n");
    b.write("app.rs", "fn app() {}\n");
    commit_all(&rb, "init");
    let head = rb.head().unwrap().peel_to_commit().unwrap();
    rb.branch("exp", &head, false).unwrap();
    b.write(".env", "DB=local\n");
    b.write("scratch.txt", "only here\n");
    let (eb, rep) = b.join(ea.project_id);
    assert_eq!(b.text(".env"), "DB=server\n");
    assert!(b.read("scratch.txt").is_none());
    let mut differing: Vec<&str> = rep.differing.iter().map(|p| p.as_str()).collect();
    differing.sort();
    assert!(
        differing.contains(&".env") && differing.contains(&"scratch.txt"),
        "{differing:?}"
    );
    let js = read_snapshot(&eb.store, eb.state.join_snapshot.unwrap()).unwrap();
    let work = eb.store.read_tree(js.work).unwrap();
    assert_eq!(
        eb.store.read_blob(work[&RelPath::new(".env")].oid).unwrap(),
        b"DB=local\n"
    );
    assert_eq!(
        eb.store
            .read_blob(work[&RelPath::new("scratch.txt")].oid)
            .unwrap(),
        b"only here\n"
    );
    let rb = git2::Repository::open(&b.root).unwrap();
    assert!(rb.find_reference("refs/heads/exp").is_ok());
}

#[test]
fn busy_git_defers_gitstate_but_syncs_files() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    std::fs::create_dir_all(&a.root).unwrap();
    let repo = init_repo(&a.root);
    a.write("f.txt", "1");
    commit_all(&repo, "init");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    std::fs::write(b.root.join(".git/index.lock"), "").unwrap();
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch("dzban", &head, false).unwrap();
    repo.set_head("refs/heads/dzban").unwrap();
    a.write("f.txt", "2");
    ea.sync().unwrap();
    eb.sync().unwrap();
    assert_eq!(b.text("f.txt"), "2");
    let rb = git2::Repository::open(&b.root).unwrap();
    assert_ne!(rb.head().unwrap().name().unwrap(), "refs/heads/dzban");
    // B's busy git must not revert A's branch switch on the server
    ea.sync().unwrap();
    assert_eq!(
        git2::Repository::open(&a.root)
            .unwrap()
            .head()
            .unwrap()
            .name()
            .unwrap(),
        "refs/heads/dzban"
    );
    std::fs::remove_file(b.root.join(".git/index.lock")).unwrap();
    eb.sync().unwrap();
    assert_eq!(
        git2::Repository::open(&b.root)
            .unwrap()
            .head()
            .unwrap()
            .name()
            .unwrap(),
        "refs/heads/dzban"
    );
}

#[test]
fn claude_chats_follow_project_with_path_rewrite() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device_at("b", &w.tmp.path().join("elsewhere/deeper/proj-b"));
    let enc = |p: &std::path::Path| spoko_core::external::encode_claude_dir(&p.to_string_lossy());
    let chat_a = a.claude.join("projects").join(enc(&a.root));
    std::fs::create_dir_all(&chat_a).unwrap();
    let line = |root: &std::path::Path| {
        format!(
            "{{\"uuid\":\"1\",\"cwd\":\"{}\",\"msg\":\"hi\"}}\n",
            root.to_string_lossy().replace('\\', "\\\\")
        )
    };
    std::fs::write(chat_a.join("s1.jsonl"), line(&a.root)).unwrap();
    a.write("x", "1");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    let chat_b = b.claude.join("projects").join(enc(&b.root));
    assert_eq!(
        std::fs::read_to_string(chat_b.join("s1.jsonl")).unwrap(),
        line(&b.root)
    );
    // B continues the chat; A sees it with its own path
    let both = format!("{}{}", line(&b.root), "{\"uuid\":\"2\",\"msg\":\"more\"}\n");
    std::fs::write(chat_b.join("s1.jsonl"), &both).unwrap();
    assert_eq!(eb.sync().unwrap(), SyncOutcome::Pushed);
    ea.sync().unwrap();
    assert_eq!(
        std::fs::read_to_string(chat_a.join("s1.jsonl")).unwrap(),
        format!("{}{}", line(&a.root), "{\"uuid\":\"2\",\"msg\":\"more\"}\n")
    );
    assert_eq!(ea.sync().unwrap(), SyncOutcome::UpToDate);
}

#[test]
fn noop_change_creates_no_snapshot() {
    let mut w = World::new();
    let a = w.device("a");
    a.write("x", "1");
    let mut ea = a.create();
    let before = ea.state.base;
    a.write("x", "1"); // rewritten, same content
    assert_eq!(ea.sync().unwrap(), SyncOutcome::UpToDate);
    assert_eq!(ea.capture(None).unwrap(), before);
    assert_eq!(ea.state.base, before);
}

#[test]
fn watched_lockfile_change_queues_hook() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    a.write(
        ".spokoconfig",
        "[[watch]]\nfiles = [\"package-lock.json\"]\nrun = \"npm ci\"\n",
    );
    a.write("package-lock.json", "{\"v\":1}");
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    assert_eq!(
        eb.state.pending_hooks,
        vec![0],
        "fresh checkout needs the install too"
    );
    assert!(
        b.notes
            .take()
            .iter()
            .any(|n| matches!(n, Notification::HookPending { .. }))
    );
    eb.state.pending_hooks.clear(); // ran (Task 23)
    a.write("README", "unrelated");
    ea.sync().unwrap();
    eb.sync().unwrap();
    assert!(
        eb.state.pending_hooks.is_empty(),
        "unrelated change runs nothing"
    );
    a.write("package-lock.json", "{\"v\":2}");
    ea.sync().unwrap();
    eb.sync().unwrap();
    assert_eq!(eb.state.pending_hooks, vec![0]);
    assert!(
        b.notes
            .take()
            .iter()
            .any(|n| matches!(n, Notification::HookPending { command, .. } if command == "npm ci"))
    );
}

#[test]
fn case_collision_entries_are_not_deleted_remotely() {
    let mut w = World::new();
    let a = w.device("a");
    let mut b = w.device("b");
    b.ctx.case_insensitive = Some(true);
    a.write("Foo.txt", "upper");
    a.write("foo.txt", "lower");
    if a.read("Foo.txt").unwrap() == b"lower" {
        // case-insensitive host: emulate a case-sensitive A by skipping (covered on Linux CI)
        return;
    }
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    assert!(
        b.read("foo.txt").is_none(),
        "second case variant not materialized"
    );
    b.write("other.txt", "x");
    eb.sync().unwrap();
    ea.sync().unwrap();
    assert_eq!(a.text("Foo.txt"), "upper");
    assert_eq!(a.text("foo.txt"), "lower");
}

#[test]
#[ignore = "perf: run in release (Task 28)"]
fn small_edit_latency_under_1s() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    for i in 0..10_000 {
        a.write(&format!("d{:02}/f{i}.txt", i % 50), format!("file {i}\n"));
    }
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    a.write("d07/f7.txt", "changed\n");
    let dirty: BTreeSet<RelPath> = [RelPath::new("d07/f7.txt")].into();
    let t = std::time::Instant::now();
    ea.sync_dirty(Some(&dirty)).unwrap();
    eb.sync_dirty(Some(&BTreeSet::new())).unwrap();
    let el = t.elapsed();
    assert_eq!(b.text("d07/f7.txt"), "changed\n");
    assert!(el < std::time::Duration::from_secs(1), "{el:?}");
}

#[test]
fn newly_ignored_paths_stay_on_disk_everywhere() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    for i in 0..60 {
        a.write(&format!("build/f{i:02}"), format!("{i}"));
    }
    for i in 0..40 {
        a.write(&format!("src/s{i:02}"), format!("{i}"));
    }
    let mut ea = a.create();
    let (mut eb, _) = b.join(ea.project_id);
    a.write(".gitignore", "build/\n");
    assert_eq!(ea.sync().unwrap(), SyncOutcome::Pushed, "not a mass delete: files are still on disk");
    assert_eq!(eb.sync().unwrap(), SyncOutcome::FastForwarded);
    assert!(b.read("build/f00").is_some(), "newly ignored file must not be deleted on B");
    assert_eq!(b.text(".gitignore"), "build/\n");
    assert_eq!(eb.sync().unwrap(), SyncOutcome::UpToDate);
    assert_eq!(ea.sync().unwrap(), SyncOutcome::UpToDate);
    // a later edit to an ignored file is not synced
    b.write("build/f00", "changed on b");
    eb.sync().unwrap();
    ea.sync().unwrap();
    assert_eq!(a.text("build/f00"), "0");
}

#[test]
fn downloaded_objects_are_known_so_next_push_skips_them() {
    let mut w = World::new();
    let a = w.device("a");
    let b = w.device("b");
    for i in 0..50 {
        a.write(&format!("f{i}"), format!("{i}"));
    }
    let ea = a.create();
    let (eb, _) = b.join(ea.project_id);
    assert!(eb.known_objects() >= 50, "clone must record downloaded ids: {}", eb.known_objects());
}
