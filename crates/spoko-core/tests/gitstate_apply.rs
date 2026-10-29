mod support;

use std::fs;
use std::path::{Path, PathBuf};

use spoko_core::gitstate::{ApplyGitOutcome, CaptureOutcome, GitState, Head, apply, capture, config_merge};
use spoko_core::store::Store;
use support::*;

struct Two {
    _d: tempfile::TempDir,
    a: PathBuf,
    b: PathBuf,
    store_a: Store,
    store_b: Store,
}

fn two() -> Two {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("a");
    let b = d.path().join("b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    let store_a = Store::open_or_init(&d.path().join("sa.git")).unwrap();
    let store_b = Store::open_or_init(&d.path().join("sb.git")).unwrap();
    Two { _d: d, a, b, store_a, store_b }
}

fn cap(root: &Path, store: &Store) -> GitState {
    match capture(root, store).unwrap() {
        CaptureOutcome::Captured(g) => g,
        o => panic!("{o:?}"),
    }
}

/// Moves objects A-store → B-store (what transfer does) and copies worktree files (what work sync does).
fn ship(t: &Two, gs: &GitState) {
    let mut roots: Vec<git2::Oid> = gs.refs.values().copied().chain(gs.stash.iter().map(|s| s.oid)).collect();
    roots.extend(gs.index_tree);
    roots.extend(gs.files.values().map(|e| e.oid));
    if let Head::Detached(o) = gs.head {
        roots.push(o);
    }
    spoko_core::gitstate::copy_objects(t.store_a.repo(), t.store_b.repo(), &roots).unwrap();
    for e in walkdir(&t.a) {
        let rel = e.strip_prefix(&t.a).unwrap();
        if rel.starts_with(".git") {
            continue;
        }
        let dst = t.b.join(rel);
        fs::create_dir_all(dst.parent().unwrap()).unwrap();
        fs::copy(&e, dst).unwrap();
    }
}

fn walkdir(p: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    for e in fs::read_dir(p).unwrap() {
        let e = e.unwrap();
        if e.file_type().unwrap().is_dir() {
            out.extend(walkdir(&e.path()));
        } else {
            out.push(e.path());
        }
    }
    out
}

fn status_clean(root: &Path) -> bool {
    let repo = git2::Repository::open(root).unwrap();
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true);
    let st = repo.statuses(Some(&mut opts)).unwrap();
    let dirty: Vec<String> = st.iter().map(|s| format!("{:?} {}", s.status(), s.path().unwrap_or("?"))).collect();
    if !dirty.is_empty() {
        eprintln!("dirty: {dirty:?}");
    }
    dirty.is_empty()
}

#[test]
fn apply_mirrors_spec_example() {
    let t = two();
    let repo = init_repo(&t.a);
    commit(&repo, &[("a.txt", "a")], "init");
    checkout_new_branch(&repo, "johniak");
    let c = commit(&repo, &[("b.txt", "b")], "on johniak");
    checkout_new_branch(&repo, "dzban");
    let gs = cap(&t.a, &t.store_a);
    ship(&t, &gs);
    assert!(matches!(apply(&t.b, &t.store_b, None, &gs).unwrap(), ApplyGitOutcome::Applied));
    let rb = git2::Repository::open(&t.b).unwrap();
    assert_eq!(rb.head().unwrap().name().unwrap(), "refs/heads/dzban");
    assert_eq!(rb.find_reference("refs/heads/johniak").unwrap().target(), Some(c));
    assert_eq!(rb.find_commit(c).unwrap().message().unwrap(), "on johniak");
    assert!(status_clean(&t.b));
}

#[test]
fn apply_staged_index() {
    let t = two();
    let repo = init_repo(&t.a);
    commit(&repo, &[("a.txt", "a")], "init");
    fs::write(t.a.join("new.txt"), "staged").unwrap();
    let mut idx = repo.index().unwrap();
    idx.add_path(Path::new("new.txt")).unwrap();
    idx.write().unwrap();
    let gs = cap(&t.a, &t.store_a);
    ship(&t, &gs);
    apply(&t.b, &t.store_b, None, &gs).unwrap();
    let rb = git2::Repository::open(&t.b).unwrap();
    let st = rb.status_file(Path::new("new.txt")).unwrap();
    assert!(st.contains(git2::Status::INDEX_NEW), "{st:?}");
}

#[test]
fn apply_stash_list_order() {
    let t = two();
    let mut repo = init_repo(&t.a);
    commit(&repo, &[("a.txt", "a")], "init");
    fs::write(t.a.join("a.txt"), "one").unwrap();
    let s1 = repo.stash_save(&sig(), "first", None).unwrap();
    fs::write(t.a.join("a.txt"), "two").unwrap();
    let s2 = repo.stash_save(&sig(), "second", None).unwrap();
    let gs = cap(&t.a, &t.store_a);
    assert_eq!(gs.stash.iter().map(|s| s.oid).collect::<Vec<_>>(), vec![s2, s1]);
    ship(&t, &gs);
    apply(&t.b, &t.store_b, None, &gs).unwrap();
    let mut rb = git2::Repository::open(&t.b).unwrap();
    let mut seen = vec![];
    rb.stash_foreach(|_, msg, oid| {
        seen.push((*oid, msg.to_owned()));
        true
    })
    .unwrap();
    assert_eq!(seen.iter().map(|s| s.0).collect::<Vec<_>>(), vec![s2, s1]);
    assert!(seen[0].1.contains("second"));
    // stash dropped on A → dropped on B
    let mut gs2 = gs.clone();
    gs2.stash.clear();
    apply(&t.b, &t.store_b, Some(&gs), &gs2).unwrap();
    assert!(rb.find_reference("refs/stash").is_err());
}

#[test]
fn apply_deletes_removed_refs_and_is_idempotent() {
    let t = two();
    let repo = init_repo(&t.a);
    commit(&repo, &[("a.txt", "a")], "init");
    checkout_new_branch(&repo, "tmp");
    checkout_new_branch(&repo, "main2");
    let gs1 = cap(&t.a, &t.store_a);
    ship(&t, &gs1);
    apply(&t.b, &t.store_b, None, &gs1).unwrap();
    repo.find_branch("tmp", git2::BranchType::Local).unwrap().delete().unwrap();
    let gs2 = cap(&t.a, &t.store_a);
    apply(&t.b, &t.store_b, Some(&gs1), &gs2).unwrap();
    let rb = git2::Repository::open(&t.b).unwrap();
    assert!(rb.find_reference("refs/heads/tmp").is_err());
    let reflog_len = rb.reflog("refs/heads/main2").unwrap().len();
    apply(&t.b, &t.store_b, Some(&gs2), &gs2).unwrap();
    assert_eq!(rb.reflog("refs/heads/main2").unwrap().len(), reflog_len, "no-op apply touches nothing");
    assert!(cap(&t.b, &t.store_b).refs == gs2.refs);
}

#[test]
fn apply_detached_and_unborn_head() {
    let t = two();
    let repo = init_repo(&t.a);
    let c1 = commit(&repo, &[("a.txt", "a")], "init");
    repo.set_head_detached(c1).unwrap();
    let gs = cap(&t.a, &t.store_a);
    ship(&t, &gs);
    apply(&t.b, &t.store_b, None, &gs).unwrap();
    let rb = git2::Repository::open(&t.b).unwrap();
    assert!(rb.head_detached().unwrap());
    let mut unborn = gs.clone();
    unborn.head = Head::Unborn("refs/heads/fresh".into());
    apply(&t.b, &t.store_b, Some(&gs), &unborn).unwrap();
    assert_eq!(rb.find_reference("HEAD").unwrap().symbolic_target().unwrap(), Some("refs/heads/fresh"));
}

#[test]
fn apply_defers_while_index_lock_present() {
    let t = two();
    let repo = init_repo(&t.a);
    commit(&repo, &[("a.txt", "a")], "init");
    let gs = cap(&t.a, &t.store_a);
    ship(&t, &gs);
    init_repo(&t.b);
    fs::write(t.b.join(".git/index.lock"), "").unwrap();
    assert!(matches!(apply(&t.b, &t.store_b, None, &gs).unwrap(), ApplyGitOutcome::Deferred(_)));
    assert!(git2::Repository::open(&t.b).unwrap().head().is_err(), "nothing applied");
}

#[test]
fn apply_refuses_worktree_git_file() {
    let t = two();
    let repo = init_repo(&t.a);
    commit(&repo, &[("a.txt", "a")], "init");
    let gs = cap(&t.a, &t.store_a);
    fs::write(t.b.join(".git"), "gitdir: /x").unwrap();
    assert!(matches!(apply(&t.b, &t.store_b, None, &gs).unwrap(), ApplyGitOutcome::Deferred(_)));
}

#[test]
fn config_merge_keeps_local_machine_keys() {
    let d = tempfile::tempdir().unwrap();
    let local = d.path().join("config");
    fs::write(&local, "[core]\n\tfilemode = false\n[credential]\n\thelper = manager\n").unwrap();
    let remote = b"[core]\n\tfilemode = true\n\tbare = false\n[remote \"origin\"]\n\turl = git@github.com:j/s.git\n";
    let merged = String::from_utf8(config_merge::merge(remote, &local).unwrap()).unwrap();
    let path = d.path().join("merged");
    fs::write(&path, &merged).unwrap();
    let cfg = git2::Config::open(&path).unwrap();
    assert!(!cfg.get_bool("core.filemode").unwrap());
    assert_eq!(cfg.get_string("credential.helper").unwrap(), "manager");
    assert_eq!(cfg.get_string("remote.origin.url").unwrap(), "git@github.com:j/s.git");
    let missing = config_merge::merge(remote, &d.path().join("nope")).unwrap();
    assert!(!String::from_utf8(missing).unwrap().contains("filemode"));
    assert!(config_merge::is_machine_local("gpg.ssh.program"));
    assert!(config_merge::is_machine_local("Credential.Helper"));
    assert!(!config_merge::is_machine_local("user.name"));
}

#[cfg(unix)]
#[test]
fn hooks_exec_bit_preserved_and_removed_hooks_deleted() {
    use std::os::unix::fs::PermissionsExt;
    let t = two();
    let repo = init_repo(&t.a);
    commit(&repo, &[("a.txt", "a")], "init");
    let hook = t.a.join(".git/hooks/pre-commit");
    fs::write(&hook, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(t.a.join(".git/info/exclude"), "secret/\n").unwrap();
    let gs = cap(&t.a, &t.store_a);
    ship(&t, &gs);
    apply(&t.b, &t.store_b, None, &gs).unwrap();
    let mode = fs::metadata(t.b.join(".git/hooks/pre-commit")).unwrap().permissions().mode();
    assert_eq!(mode & 0o111, 0o111);
    assert_eq!(fs::read_to_string(t.b.join(".git/info/exclude")).unwrap(), "secret/\n");
    fs::remove_file(&hook).unwrap();
    let gs2 = cap(&t.a, &t.store_a);
    apply(&t.b, &t.store_b, Some(&gs), &gs2).unwrap();
    assert!(!t.b.join(".git/hooks/pre-commit").exists());
}

#[test]
fn apply_inits_repo_when_missing_with_remote_config() {
    let t = two();
    let repo = init_repo(&t.a);
    repo.remote("origin", "https://example.com/r.git").unwrap();
    commit(&repo, &[("a.txt", "a")], "init");
    let gs = cap(&t.a, &t.store_a);
    ship(&t, &gs);
    apply(&t.b, &t.store_b, None, &gs).unwrap();
    let rb = git2::Repository::open(&t.b).unwrap();
    assert_eq!(rb.find_remote("origin").unwrap().url().unwrap(), "https://example.com/r.git");
}
