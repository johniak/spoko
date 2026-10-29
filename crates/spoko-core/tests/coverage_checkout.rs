#![cfg_attr(windows, allow(unused_imports, dead_code))] // helpers of #[cfg(unix)] tests
use std::fs;

use git2::Oid;
use spoko_core::checkout::{ApplyError, ApplyOpts, ApplyReport, SkipReason, apply_tree};
use spoko_core::external::PathRewrite;
use spoko_core::platform::{NameProblem, RelPath, fs_is_case_insensitive};
use spoko_core::scan::StatCache;
use spoko_core::store::{Entry, EntryMode, Store, WorkMap};

struct Env {
    dir: tempfile::TempDir,
    store: Store,
    cache: StatCache,
}

impl Env {
    fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("p")).unwrap();
        let store = Store::open_or_init(&dir.path().join("s.git")).unwrap();
        Env { dir, store, cache: StatCache::default() }
    }
    fn root(&self) -> std::path::PathBuf {
        self.dir.path().join("p")
    }
    fn entry(&self, content: &[u8], mode: EntryMode) -> Entry {
        Entry { oid: self.store.write_blob(content).unwrap(), mode }
    }
    fn map(&self, entries: &[(&str, Entry)]) -> WorkMap {
        let mut w = WorkMap::default();
        for (p, e) in entries {
            w.insert(RelPath::new(p), *e);
        }
        w
    }
    fn apply(&mut self, from: &WorkMap, to: &WorkMap, opts: &ApplyOpts) -> Result<ApplyReport, ApplyError> {
        let root = self.root();
        apply_tree(&root, &self.store, &mut self.cache, from, to, opts)
    }
    fn write(&self, rel: &str, data: &[u8]) {
        let p = self.root().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, data).unwrap();
    }
}

fn gitlink(n: u8) -> Entry {
    Entry { oid: Oid::from_bytes(&[n; 20]).unwrap(), mode: EntryMode::Commit }
}

#[test]
fn opts_for_root_probe_the_filesystem() {
    let d = tempfile::tempdir().unwrap();
    let o = ApplyOpts::for_root(d.path()).unwrap();
    assert_eq!(o.case_insensitive, fs_is_case_insensitive(d.path()).unwrap());
    assert_eq!(o.windows_names, cfg!(windows));
    assert!(o.faults.is_none() && o.transform.is_none());
    assert_eq!(fs::read_dir(d.path()).unwrap().count(), 0, "probe file removed");
    assert!(ApplyOpts::for_root(&d.path().join("missing")).is_err());
}

#[test]
fn skipped_paths_explain_why() {
    let mut e = Env::new();
    e.write("x/untracked", b"mine");
    let blob = e.entry(b"c", EntryMode::Blob);
    let to = e.map(&[("a:b", blob), ("x", blob), ("ok", blob)]);
    let r = e.apply(&WorkMap::default(), &to, &ApplyOpts { windows_names: true, ..Default::default() }).unwrap();
    assert_eq!(r.written, vec![RelPath::new("ok")]);
    assert_eq!(
        r.skipped,
        vec![(RelPath::new("a:b"), SkipReason::WindowsName(NameProblem::InvalidChar(':'))), (RelPath::new("x"), SkipReason::DirInTheWay)]
    );
    assert_eq!(r.skipped[0].1.to_string(), "znak ':' jest niedozwolony na Windows");
    assert_eq!(r.skipped[1].1.to_string(), "w tym miejscu jest katalog z nieśledzonymi plikami");
    assert_eq!(fs::read(e.root().join("x/untracked")).unwrap(), b"mine");
    assert!(!e.root().join("a:b").exists());
}

#[test]
fn missing_blob_in_store_is_a_git_error() {
    let mut e = Env::new();
    let to = e.map(&[("a", Entry { oid: Oid::from_bytes(&[7; 20]).unwrap(), mode: EntryMode::Blob })]);
    match e.apply(&WorkMap::default(), &to, &ApplyOpts::default()) {
        Err(ApplyError::Core(spoko_core::Error::Git(g))) => assert_eq!(g.code(), git2::ErrorCode::NotFound),
        other => panic!("{other:?}"),
    }
    assert!(!e.root().join("a").exists());
}

#[test]
fn moved_gitlink_over_checked_out_submodule_touches_nothing() {
    let mut e = Env::new();
    e.write("sub/inner.txt", b"submodule content");
    let from = e.map(&[("sub", gitlink(1))]);
    let to = e.map(&[("sub", gitlink(2))]);
    let r = e.apply(&from, &to, &ApplyOpts::default()).unwrap();
    assert_eq!(r, ApplyReport::default());
    assert_eq!(fs::read(e.root().join("sub/inner.txt")).unwrap(), b"submodule content");
}

#[test]
fn removed_gitlink_keeps_submodule_directory() {
    let mut e = Env::new();
    e.write("sub/inner.txt", b"submodule content");
    let from = e.map(&[("sub", gitlink(1))]);
    let r = e.apply(&from, &WorkMap::default(), &ApplyOpts::default()).unwrap();
    assert!(r.deleted.is_empty());
    assert_eq!(fs::read(e.root().join("sub/inner.txt")).unwrap(), b"submodule content");
}

#[test]
fn directory_where_a_file_was_expected_is_a_local_change() {
    let mut e = Env::new();
    e.write("x/new.txt", b"made locally");
    let from = e.map(&[("x", e.entry(b"old", EntryMode::Blob))]);
    match e.apply(&from, &WorkMap::default(), &ApplyOpts::default()) {
        Err(ApplyError::LocalChanged(paths)) => assert_eq!(paths, vec![RelPath::new("x")]),
        other => panic!("{other:?}"),
    }
    assert!(e.root().join("x/new.txt").exists());
}

#[test]
fn transformed_file_is_compared_in_snapshot_form() {
    let mut e = Env::new();
    let t = PathRewrite { root: "/home/me/p".into() };
    let opts = ApplyOpts { transform: Some(&t), ..Default::default() };
    e.write("c.jsonl", br#"{"cwd":"/home/me/p/src"}"#);
    let from = e.map(&[("c.jsonl", e.entry(br#"{"cwd":"${SPOKO_PROJECT_ROOT}/src"}"#, EntryMode::Blob))]);
    let to = e.map(&[("c.jsonl", e.entry(br#"{"cwd":"${SPOKO_PROJECT_ROOT}/lib"}"#, EntryMode::Blob))]);
    let r = e.apply(&from, &to, &opts).unwrap();
    assert_eq!(r.written, vec![RelPath::new("c.jsonl")]);
    assert_eq!(fs::read(e.root().join("c.jsonl")).unwrap(), br#"{"cwd":"/home/me/p/lib"}"#);

    e.cache = StatCache::default();
    e.write("c.jsonl", br#"{"cwd":"/home/me/p/edited"}"#);
    assert!(matches!(e.apply(&to, &from, &opts), Err(ApplyError::LocalChanged(p)) if p == vec![RelPath::new("c.jsonl")]));
}

#[cfg(unix)]
#[test]
fn symlink_on_disk_is_compared_by_target() {
    let mut e = Env::new();
    std::os::unix::fs::symlink("t1", e.root().join("l")).unwrap();
    let from = e.map(&[("l", e.entry(b"t1", EntryMode::Link))]);
    let to = e.map(&[("l", e.entry(b"t2", EntryMode::Link))]);
    e.apply(&from, &to, &ApplyOpts::default()).unwrap();
    assert_eq!(fs::read_link(e.root().join("l")).unwrap().to_str(), Some("t2"));

    e.cache = StatCache::default();
    fs::remove_file(e.root().join("l")).unwrap();
    std::os::unix::fs::symlink("elsewhere", e.root().join("l")).unwrap();
    assert!(matches!(e.apply(&to, &from, &ApplyOpts::default()), Err(ApplyError::LocalChanged(p)) if p == vec![RelPath::new("l")]));
}

#[test]
fn pooled_writes_flush_in_waves_of_64_mib() {
    let mut e = Env::new();
    let blob = e.entry(&vec![b'z'; 1 << 20], EntryMode::Blob);
    let names: Vec<String> = (0..65).map(|i| format!("d{}/f{i}", i % 4)).collect();
    let mut to = WorkMap::default();
    for n in &names {
        to.insert(RelPath::new(n), blob);
    }
    let r = e.apply(&WorkMap::default(), &to, &ApplyOpts::default()).unwrap();
    assert_eq!(r.written.len(), 65);
    for n in &names {
        assert_eq!(fs::metadata(e.root().join(n)).unwrap().len(), 1 << 20, "{n}");
    }
    assert_eq!(e.cache.entries.len(), 65);
    assert!(e.cache.entries.values().all(|c| c.entry == blob));
}

#[test]
fn streamed_write_under_a_file_fails_without_touching_it() {
    let mut e = Env::new();
    e.write("blocker", b"untracked file");
    let big = e.entry(&vec![b'q'; (1 << 20) + 1], EntryMode::Blob);
    let to = e.map(&[("blocker/big.bin", big)]);
    match e.apply(&WorkMap::default(), &to, &ApplyOpts::default()) {
        Err(ApplyError::Core(spoko_core::Error::Io(_))) => {}
        other => panic!("{other:?}"),
    }
    assert_eq!(fs::read(e.root().join("blocker")).unwrap(), b"untracked file");
}
