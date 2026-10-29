mod support;

use std::fs;

use spoko_core::gitstate::{BusyReason, CaptureOutcome, Head, capture, gitstate_tree, read_gitstate};
use spoko_core::store::Store;
use support::*;

fn env() -> (tempfile::TempDir, std::path::PathBuf, Store) {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("p");
    fs::create_dir(&root).unwrap();
    let store = Store::open_or_init(&d.path().join("s.git")).unwrap();
    (d, root, store)
}

fn captured(root: &std::path::Path, store: &Store) -> spoko_core::gitstate::GitState {
    match capture(root, store).unwrap() {
        CaptureOutcome::Captured(gs) => gs,
        other => panic!("{other:?}"),
    }
}

#[test]
fn no_repo() {
    let (_d, root, store) = env();
    assert!(matches!(capture(&root, &store).unwrap(), CaptureOutcome::NoRepo));
}

#[test]
fn captures_branch_refs_index_stash() {
    let (_d, root, store) = env();
    let mut repo = init_repo(&root);
    let c1 = commit(&repo, &[("a.txt", "a")], "first");
    checkout_new_branch(&repo, "johniak");
    let c2 = commit(&repo, &[("b.txt", "b")], "second");
    fs::write(root.join("a.txt"), "stash me").unwrap();
    let stash = repo.stash_save(&sig(), "wip", None).unwrap();
    fs::write(root.join("staged.txt"), "staged content").unwrap();
    let mut idx = repo.index().unwrap();
    idx.add_path(std::path::Path::new("staged.txt")).unwrap();
    idx.write().unwrap();

    let gs = captured(&root, &store);
    assert_eq!(gs.head, Head::Branch("refs/heads/johniak".into()));
    assert_eq!(gs.refs["refs/heads/johniak"], c2);
    assert_eq!(gs.refs.values().filter(|o| **o == c1).count(), 1);
    assert!(!gs.refs.contains_key("refs/stash"));
    assert_eq!(gs.stash.len(), 1);
    assert_eq!(gs.stash[0].oid, stash);
    assert!(gs.stash[0].message.contains("wip"));
    let index = store.read_tree(gs.index_tree.unwrap()).unwrap();
    let staged = index[&spoko_core::platform::RelPath::new("staged.txt")];
    assert_eq!(store.read_blob(staged.oid).unwrap(), b"staged content");
    assert!(store.has(c1) && store.has(c2) && store.has(stash));
    assert!(gs.files.contains_key("config"));
    let cfg = String::from_utf8(store.read_blob(gs.files["config"].oid).unwrap()).unwrap();
    assert!(!cfg.contains("filemode"), "machine-local keys stripped: {cfg}");
}

#[test]
fn hooks_and_exclude_captured_samples_skipped() {
    let (_d, root, store) = env();
    let repo = init_repo(&root);
    commit(&repo, &[("a", "a")], "c");
    fs::create_dir_all(root.join(".git/hooks")).unwrap();
    fs::write(root.join(".git/hooks/pre-commit"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::write(root.join(".git/hooks/pre-push.sample"), "sample").unwrap();
    fs::create_dir_all(root.join(".git/info")).unwrap();
    fs::write(root.join(".git/info/exclude"), "local/\n").unwrap();
    fs::create_dir_all(root.join(".git/lfs/objects/ab/cd")).unwrap();
    fs::write(root.join(".git/lfs/objects/ab/cd/abcd1234"), "lfs blob").unwrap();
    let gs = captured(&root, &store);
    assert!(gs.files.contains_key("hooks/pre-commit"));
    assert!(!gs.files.contains_key("hooks/pre-push.sample"));
    assert!(gs.files.contains_key("info/exclude"));
    assert!(gs.files.contains_key("lfs/objects/ab/cd/abcd1234"));
}

#[test]
fn detached_head() {
    let (_d, root, store) = env();
    let repo = init_repo(&root);
    let c1 = commit(&repo, &[("a", "1")], "c1");
    commit(&repo, &[("a", "2")], "c2");
    repo.set_head_detached(c1).unwrap();
    assert_eq!(captured(&root, &store).head, Head::Detached(c1));
}

#[test]
fn unborn_branch() {
    let (_d, root, store) = env();
    init_repo(&root);
    let gs = captured(&root, &store);
    assert!(matches!(gs.head, Head::Unborn(ref b) if b.starts_with("refs/heads/")));
    assert!(gs.refs.is_empty());
}

#[test]
fn busy_on_index_lock() {
    let (_d, root, store) = env();
    let repo = init_repo(&root);
    commit(&repo, &[("a", "1")], "c1");
    fs::write(root.join(".git/index.lock"), "").unwrap();
    assert!(matches!(capture(&root, &store).unwrap(), CaptureOutcome::Busy(BusyReason::Lock(_))));
    fs::remove_file(root.join(".git/index.lock")).unwrap();
    fs::write(root.join(".git/refs/heads/x.lock"), "").unwrap();
    assert!(matches!(capture(&root, &store).unwrap(), CaptureOutcome::Busy(BusyReason::Lock(_))));
}

#[test]
fn busy_on_rebase_and_merge() {
    let (_d, root, store) = env();
    let repo = init_repo(&root);
    commit(&repo, &[("a", "1")], "c1");
    fs::create_dir(root.join(".git/rebase-merge")).unwrap();
    assert!(matches!(capture(&root, &store).unwrap(), CaptureOutcome::Busy(BusyReason::InProgress("rebase-merge"))));
    fs::remove_dir(root.join(".git/rebase-merge")).unwrap();
    fs::write(root.join(".git/MERGE_HEAD"), "x").unwrap();
    assert!(matches!(capture(&root, &store).unwrap(), CaptureOutcome::Busy(BusyReason::InProgress("MERGE_HEAD"))));
}

#[test]
fn unsupported_worktree_file_and_sha256() {
    let (_d, root, store) = env();
    fs::write(root.join(".git"), "gitdir: /elsewhere").unwrap();
    assert!(matches!(capture(&root, &store).unwrap(), CaptureOutcome::Unsupported("worktree")));
    fs::remove_file(root.join(".git")).unwrap();
    init_repo(&root);
    let cfg = root.join(".git/config");
    let mut text = fs::read_to_string(&cfg).unwrap();
    text.push_str("[extensions]\n\tobjectformat = sha256\n");
    fs::write(&cfg, text).unwrap();
    assert!(matches!(capture(&root, &store).unwrap(), CaptureOutcome::Unsupported("sha256")));
}

#[test]
fn objects_copied_once() {
    let (_d, root, store) = env();
    let repo = init_repo(&root);
    for i in 0..5 {
        commit(&repo, &[("f", &i.to_string())], "c");
    }
    let first = spoko_core::gitstate::capture_with_stats(&root, &store).unwrap().1;
    assert!(first.objects_copied >= 15, "{first:?}");
    let second = spoko_core::gitstate::capture_with_stats(&root, &store).unwrap().1;
    assert_eq!(second.objects_copied, 0);
}

#[test]
fn gitstate_tree_roundtrip() {
    let (_d, root, store) = env();
    let repo = init_repo(&root);
    commit(&repo, &[("a", "1")], "c1");
    fs::create_dir_all(root.join(".git/hooks")).unwrap();
    fs::write(root.join(".git/hooks/post-commit"), "#!/bin/sh").unwrap();
    let gs = captured(&root, &store);
    let tree = gitstate_tree(&store, &gs).unwrap();
    assert_eq!(read_gitstate(&store, tree).unwrap(), Some(gs.clone()));
    let empty = store.build_tree(&Default::default()).unwrap();
    assert_eq!(read_gitstate(&store, empty).unwrap(), None);
    // tips gitlinks make history reachable from the tree
    let all = store.read_tree(tree).unwrap();
    let head_oid = *gs.refs.values().next().unwrap();
    assert!(all.iter().any(|(p, e)| p.as_str().starts_with("tips/") && e.mode == spoko_core::store::EntryMode::Commit && e.oid == head_oid));
}
