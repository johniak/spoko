mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use git2::{IndexEntry, IndexTime, Oid, Repository};
use spoko_core::gitstate::{
    ApplyGitOutcome, BusyReason, CaptureOutcome, GitMergeNote, GitState, Head, StashEntry, apply, capture, config_merge, gitstate_tree, merge_gitstate,
    read_gitstate,
};
use spoko_core::platform::RelPath;
use spoko_core::store::{Entry, EntryMode, Store, WorkMap};
use support::*;

struct Env {
    _d: tempfile::TempDir,
    a: PathBuf,
    b: PathBuf,
    store: Store,
}

fn env() -> Env {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("a");
    let b = d.path().join("b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    let store = Store::open_or_init(&d.path().join("s.git")).unwrap();
    Env { _d: d, a, b, store }
}

fn cap(root: &Path, store: &Store) -> GitState {
    match capture(root, store).unwrap() {
        CaptureOutcome::Captured(g) => g,
        o => panic!("{o:?}"),
    }
}

fn applied(root: &Path, store: &Store, from: Option<&GitState>, to: &GitState) {
    assert!(matches!(apply(root, store, from, to).unwrap(), ApplyGitOutcome::Applied));
}

fn bare(head: Head, refs: &[(&str, Oid)]) -> GitState {
    GitState { head, refs: refs.iter().map(|(n, o)| ((*n).to_owned(), *o)).collect(), index_tree: None, stash: vec![], files: BTreeMap::new() }
}

fn index_entry(repo: &Repository, path: &str) -> IndexEntry {
    repo.index().unwrap().get_path(Path::new(path), 0).unwrap_or_else(|| panic!("{path} not in index"))
}

fn raw_entry(path: &str, id: Oid) -> IndexEntry {
    IndexEntry {
        ctime: IndexTime::new(0, 0),
        mtime: IndexTime::new(0, 0),
        dev: 0,
        ino: 0,
        mode: 0o100644,
        uid: 0,
        gid: 0,
        file_size: 0,
        id,
        flags: 0,
        flags_extended: 0,
        path: path.as_bytes().to_vec(),
    }
}

/// On Windows `file\.git` is simply "not found"; on unix it is ENOTDIR.
#[cfg(unix)]
#[test]
fn apply_into_a_file_path_is_an_io_error() {
    let e = env();
    let file = e.a.join("file");
    fs::write(&file, "x").unwrap();
    match apply(&file, &e.store, None, &bare(Head::Unborn("refs/heads/main".into()), &[])) {
        Err(spoko_core::Error::Io(err)) => assert_eq!(err.kind(), std::io::ErrorKind::NotADirectory),
        other => panic!("{other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn capture_of_a_file_path_is_an_io_error() {
    let e = env();
    let file = e.a.join("file");
    fs::write(&file, "x").unwrap();
    match capture(&file, &e.store) {
        Err(spoko_core::Error::Io(err)) => assert_eq!(err.kind(), std::io::ErrorKind::NotADirectory),
        other => panic!("{other:?}"),
    }
}

#[test]
fn detached_head_without_index_applies_and_reapplies_as_noop() {
    let e = env();
    let repo = init_repo(&e.a);
    let c = commit(&repo, &[("a.txt", "a")], "init");
    cap(&e.a, &e.store);
    let to = bare(Head::Detached(c), &[("refs/heads/main", c)]);
    let from = bare(Head::Detached(c), &[("refs/heads/main", c), ("refs/heads/ghost", c)]);
    applied(&e.b, &e.store, Some(&from), &to);
    let rb = Repository::open(&e.b).unwrap();
    assert!(rb.head_detached().unwrap());
    assert_eq!(rb.head().unwrap().target(), Some(c));
    assert_eq!(rb.find_reference("refs/heads/main").unwrap().target(), Some(c));
    assert!(rb.find_reference("refs/heads/ghost").is_err());
    assert_eq!(rb.index().unwrap().len(), 0, "no index tree: index untouched");
    let head_log = rb.reflog("HEAD").unwrap().len();
    applied(&e.b, &e.store, Some(&to), &to);
    assert_eq!(rb.reflog("HEAD").unwrap().len(), head_log, "same detached HEAD is not rewritten");
}

#[test]
fn index_keeps_unchanged_stat_data_and_fills_matching_files() {
    let e = env();
    let repo = init_repo(&e.a);
    commit(&repo, &[("a.txt", "aaa"), ("b.txt", "b1"), ("d/x.txt", "x")], "init");
    let gs1 = cap(&e.a, &e.store);
    fs::write(e.b.join("a.txt"), "aaa").unwrap();
    fs::write(e.b.join("b.txt"), "local edit").unwrap();
    applied(&e.b, &e.store, None, &gs1);
    let rb = Repository::open(&e.b).unwrap();
    let a1 = index_entry(&rb, "a.txt");
    assert_eq!(a1.file_size, 3, "worktree file matches: stat filled");
    assert_ne!(a1.mtime.seconds(), 0);
    let b1 = index_entry(&rb, "b.txt");
    assert_eq!((b1.file_size, b1.mtime.seconds()), (0, 0), "worktree file differs: stat left empty");
    let x1 = index_entry(&rb, "d/x.txt");
    assert_eq!((x1.file_size, x1.mtime.seconds()), (0, 0), "missing file: stat left empty");

    let past = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    fs::File::options().write(true).open(e.b.join("a.txt")).unwrap().set_modified(past).unwrap();
    fs::write(e.a.join("b.txt"), "b2").unwrap();
    let mut idx = repo.index().unwrap();
    idx.add_path(Path::new("b.txt")).unwrap();
    idx.write().unwrap();
    let gs2 = cap(&e.a, &e.store);
    assert_ne!(gs1.index_tree, gs2.index_tree);
    applied(&e.b, &e.store, Some(&gs1), &gs2);
    let rb = Repository::open(&e.b).unwrap();
    let a2 = index_entry(&rb, "a.txt");
    assert_eq!((a2.mtime.seconds(), a2.mtime.nanoseconds(), a2.file_size), (a1.mtime.seconds(), a1.mtime.nanoseconds(), 3), "unchanged entry kept as is");
    assert_eq!(index_entry(&rb, "b.txt").id, index_entry(&repo, "b.txt").id);
    assert_eq!(rb.index().unwrap().len(), 3);
}

#[cfg(unix)]
#[test]
fn index_fills_stat_data_for_matching_symlink() {
    let e = env();
    let repo = init_repo(&e.a);
    fs::write(e.a.join("t.txt"), "t").unwrap();
    std::os::unix::fs::symlink("t.txt", e.a.join("l")).unwrap();
    commit(&repo, &[], "init");
    let gs = cap(&e.a, &e.store);
    std::os::unix::fs::symlink("t.txt", e.b.join("l")).unwrap();
    applied(&e.b, &e.store, None, &gs);
    let l = index_entry(&Repository::open(&e.b).unwrap(), "l");
    assert_eq!(l.mode, 0o120000);
    assert_eq!(l.file_size, 5, "length of the link target");
    assert_ne!(l.mtime.seconds(), 0);
}

#[test]
fn changed_stash_list_replaces_existing_reflog() {
    let e = env();
    let mut repo = init_repo(&e.a);
    commit(&repo, &[("a.txt", "a")], "init");
    fs::write(e.a.join("a.txt"), "one").unwrap();
    let s1 = repo.stash_save(&sig(), "first", None).unwrap();
    let gs1 = cap(&e.a, &e.store);
    applied(&e.b, &e.store, None, &gs1);
    fs::write(e.a.join("a.txt"), "two").unwrap();
    let s2 = repo.stash_save(&sig(), "second", None).unwrap();
    let gs2 = cap(&e.a, &e.store);
    applied(&e.b, &e.store, Some(&gs1), &gs2);
    let rb = Repository::open(&e.b).unwrap();
    let log = rb.reflog("refs/stash").unwrap();
    let seen: Vec<(Oid, String)> = log.iter().map(|x| (x.id_new(), x.message().unwrap().unwrap().to_owned())).collect();
    assert_eq!(seen.iter().map(|s| s.0).collect::<Vec<_>>(), vec![s2, s1]);
    assert_eq!(seen.iter().map(|s| s.1.clone()).collect::<Vec<_>>(), gs2.stash.iter().map(|s| s.message.clone()).collect::<Vec<_>>());
}

#[test]
fn index_conflicts_make_capture_wait() {
    let e = env();
    let repo = init_repo(&e.a);
    commit(&repo, &[("a.txt", "a")], "init");
    let (o, u, t) = (repo.blob(b"base").unwrap(), repo.blob(b"ours").unwrap(), repo.blob(b"theirs").unwrap());
    let mut idx = repo.index().unwrap();
    for (stage, id) in [(1u16, o), (2, u), (3, t)] {
        idx.add(&IndexEntry { flags: stage << 12, ..raw_entry("c.txt", id) }).unwrap();
    }
    assert!(idx.has_conflicts());
    idx.write().unwrap();
    assert!(matches!(capture(&e.a, &e.store).unwrap(), CaptureOutcome::Busy(BusyReason::InProgress("index conflicts"))));
}

#[test]
fn index_entry_without_its_object_fails_capture() {
    let e = env();
    let repo = init_repo(&e.a);
    commit(&repo, &[("a.txt", "a")], "init");
    let lost = repo.blob(b"lost blob").unwrap();
    let mut idx = repo.index().unwrap();
    idx.add(&raw_entry("lost.txt", lost)).unwrap();
    idx.write().unwrap();
    let hex = lost.to_string();
    let loose = e.a.join(".git/objects").join(&hex[..2]).join(&hex[2..]);
    let mut perms = fs::metadata(&loose).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false); // Windows refuses to delete read-only files
    fs::set_permissions(&loose, perms).unwrap();
    fs::remove_file(&loose).unwrap();
    match capture(&e.a, &e.store) {
        Err(spoko_core::Error::Git(_)) => {}
        other => panic!("{other:?}"),
    }
    assert!(!e.store.has(lost));
}

#[test]
fn annotated_tags_are_captured_with_their_target() {
    let e = env();
    let repo = init_repo(&e.a);
    let c = commit(&repo, &[("a.txt", "a")], "init");
    let tag = repo.tag("v1", &repo.find_object(c, None).unwrap(), &sig(), "release", false).unwrap();
    let gs = cap(&e.a, &e.store);
    assert_eq!(gs.refs["refs/tags/v1"], tag);
    let stored = e.store.repo().find_tag(tag).unwrap();
    assert_eq!(stored.target_id(), c);
    assert_eq!(stored.name_bytes(), b"v1");
    assert!(e.store.repo().find_commit(c).is_ok());
}

#[test]
fn capture_without_config_or_exclude_files() {
    let e = env();
    let repo = init_repo(&e.a);
    commit(&repo, &[("a.txt", "a")], "init");
    drop(repo);
    fs::remove_file(e.a.join(".git/config")).unwrap();
    let _ = fs::remove_file(e.a.join(".git/info/exclude"));
    let gs = cap(&e.a, &e.store);
    assert!(!gs.files.contains_key("config"));
    assert!(!gs.files.contains_key("info/exclude"));
    assert!(gs.refs.contains_key("refs/heads/master") || gs.refs.contains_key("refs/heads/main"));
}

#[test]
fn branch_moved_by_us_and_deleted_by_them_is_kept() {
    let (x, y) = (Oid::from_bytes(&[1; 20]).unwrap(), Oid::from_bytes(&[2; 20]).unwrap());
    let head = Head::Branch("refs/heads/main".into());
    let base = bare(head.clone(), &[("refs/heads/f", x)]);
    let moved = bare(head.clone(), &[("refs/heads/f", y)]);
    let deleted = bare(head, &[]);
    let r = merge_gitstate(Some(&base), &moved, &deleted, false, "laptop");
    assert_eq!(r.state.refs.get("refs/heads/f"), Some(&y));
    assert_eq!(r.notes, Vec::<GitMergeNote>::new());
    let r = merge_gitstate(Some(&base), &deleted, &moved, true, "laptop");
    assert_eq!(r.state.refs.get("refs/heads/f"), Some(&y));
}

#[test]
fn detached_head_commit_is_kept_as_tip_and_roundtrips() {
    let e = env();
    let repo = init_repo(&e.a);
    let c = commit(&repo, &[("a.txt", "a")], "init");
    cap(&e.a, &e.store);
    let gs = bare(Head::Detached(c), &[]);
    let tree = gitstate_tree(&e.store, &gs).unwrap();
    let tips = e.store.subtree(tree, "tips").unwrap().unwrap();
    let tips = e.store.repo().find_tree(tips).unwrap();
    assert_eq!(tips.iter().map(|t| t.id()).collect::<Vec<_>>(), vec![c]);
    assert_eq!(e.store.subtree(tree, "files").unwrap(), None);
    assert_eq!(read_gitstate(&e.store, tree).unwrap(), Some(gs));
}

#[test]
fn config_merge_with_unreadable_local_config_fails() {
    let d = tempfile::tempdir().unwrap();
    match config_merge::merge(b"[user]\n\tname = Jan\n", d.path()) {
        Err(spoko_core::Error::Io(err)) => assert_ne!(err.kind(), std::io::ErrorKind::NotFound),
        other => panic!("{other:?}"),
    }
}

#[test]
fn stash_reflog_is_signed_by_spoko_without_a_user_identity() {
    let e = env();
    let mut repo = init_repo(&e.a);
    commit(&repo, &[("a.txt", "a")], "init");
    fs::write(e.a.join("a.txt"), "one").unwrap();
    let s1 = repo.stash_save(&sig(), "first", None).unwrap();
    let gs = cap(&e.a, &e.store);
    let rb = Repository::init(&e.b).unwrap();
    rb.config().unwrap().set_str("user.name", "").unwrap();
    rb.config().unwrap().set_str("user.email", "").unwrap();
    assert!(rb.signature().is_err());
    applied(&e.b, &e.store, None, &gs);
    let log = Repository::open(&e.b).unwrap().reflog("refs/stash").unwrap();
    assert_eq!(log.len(), 1);
    let entry = log.get(0).unwrap();
    assert_eq!(entry.id_new(), s1);
    assert_eq!(entry.committer().name().unwrap(), "spoko");
    assert_eq!(entry.committer().email().unwrap(), "spoko@localhost");
}

#[test]
fn stash_dropped_on_one_side_is_dropped_in_merge() {
    let stash = |n: u8| StashEntry { oid: Oid::from_bytes(&[n; 20]).unwrap(), message: format!("s{n}") };
    let head = Head::Branch("refs/heads/main".into());
    let with = |s: Vec<StashEntry>| GitState { stash: s, ..bare(head.clone(), &[]) };
    let base = with(vec![stash(1), stash(2), stash(3)]);
    let ours = with(vec![stash(1), stash(3)]);
    let theirs = with(vec![stash(4), stash(1), stash(2)]);
    let r = merge_gitstate(Some(&base), &ours, &theirs, true, "laptop");
    assert_eq!(r.state.stash, vec![stash(1), stash(4)]);
}

#[test]
fn index_tree_with_missing_subtree_fails_apply() {
    let e = env();
    let mut w = WorkMap::default();
    w.insert(RelPath::new("d"), Entry { oid: Oid::from_bytes(&[9; 20]).unwrap(), mode: EntryMode::Tree });
    let tree = e.store.build_tree(&w).unwrap();
    let gs = GitState { index_tree: Some(tree), ..bare(Head::Unborn("refs/heads/main".into()), &[]) };
    match apply(&e.b, &e.store, None, &gs) {
        Err(spoko_core::Error::Git(g)) => assert_eq!(g.code(), git2::ErrorCode::NotFound),
        other => panic!("{other:?}"),
    }
    assert_eq!(Repository::open(&e.b).unwrap().index().unwrap().len(), 0);
}

#[test]
fn git_file_whose_blob_is_missing_fails_apply_and_leaves_no_file() {
    let e = env();
    let missing = Entry { oid: Oid::from_bytes(&[8; 20]).unwrap(), mode: EntryMode::BlobExec };
    let gs = GitState { files: [("hooks/pre-commit".to_owned(), missing)].into(), ..bare(Head::Unborn("refs/heads/main".into()), &[]) };
    assert!(apply(&e.b, &e.store, None, &gs).is_err());
    let hooks: Vec<String> = fs::read_dir(e.b.join(".git/hooks")).unwrap().flatten().map(|d| d.file_name().to_string_lossy().into_owned()).collect();
    assert!(!hooks.iter().any(|h| h == "pre-commit" || h.starts_with(".spoko-tmp-")), "{hooks:?}");
}
