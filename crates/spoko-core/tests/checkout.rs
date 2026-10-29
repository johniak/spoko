use std::fs;
use std::io;

use spoko_core::checkout::{ApplyError, ApplyFaults, ApplyOpts, SkipReason, apply_tree};
use spoko_core::config::ProjectConfig;
use spoko_core::filter::Matcher;
use spoko_core::platform::{NameProblem, RelPath};
use spoko_core::scan::{StatCache, full_scan, incremental_scan};
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
    fn map(&self, files: &[(&str, &[u8])]) -> WorkMap {
        let mut w = WorkMap::default();
        for (p, c) in files {
            w.insert(RelPath::new(p), Entry { oid: self.store.write_blob(c).unwrap(), mode: EntryMode::Blob });
        }
        w
    }
    fn apply(&mut self, from: &WorkMap, to: &WorkMap, opts: &ApplyOpts) -> Result<spoko_core::checkout::ApplyReport, ApplyError> {
        let root = self.root();
        apply_tree(&root, &self.store, &mut self.cache, from, to, opts)
    }
    fn read(&self, p: &str) -> Vec<u8> {
        fs::read(self.root().join(p)).unwrap()
    }
}

#[test]
fn apply_writes_and_deletes() {
    let mut e = Env::new();
    let empty = WorkMap::default();
    let v1 = e.map(&[("a.txt", b"a"), ("d/b.txt", b"b")]);
    let r = e.apply(&empty, &v1, &ApplyOpts::default()).unwrap();
    assert_eq!(r.written.len(), 2);
    assert_eq!(e.read("d/b.txt"), b"b");
    let v2 = e.map(&[("a.txt", b"a2")]);
    let r = e.apply(&v1, &v2, &ApplyOpts::default()).unwrap();
    assert_eq!(r.deleted, vec![RelPath::new("d/b.txt")]);
    assert_eq!(e.read("a.txt"), b"a2");
    assert!(!e.root().join("d").exists(), "empty dir removed");
}

#[test]
fn apply_aborts_when_local_changed_without_writing() {
    let mut e = Env::new();
    let v1 = e.map(&[("a.txt", b"a"), ("b.txt", b"b")]);
    e.apply(&WorkMap::default(), &v1, &ApplyOpts::default()).unwrap();
    fs::write(e.root().join("b.txt"), b"local edit").unwrap();
    fs::write(e.root().join("new.txt"), b"local new").unwrap();
    let v2 = e.map(&[("a.txt", b"A"), ("b.txt", b"B"), ("new.txt", b"remote new")]);
    match e.apply(&v1, &v2, &ApplyOpts::default()) {
        Err(ApplyError::LocalChanged(paths)) => assert_eq!(paths, vec![RelPath::new("b.txt"), RelPath::new("new.txt")]),
        other => panic!("{other:?}"),
    }
    assert_eq!(e.read("a.txt"), b"a", "nothing written");
}

#[test]
fn apply_detects_local_delete() {
    let mut e = Env::new();
    let v1 = e.map(&[("a.txt", b"a")]);
    e.apply(&WorkMap::default(), &v1, &ApplyOpts::default()).unwrap();
    fs::remove_file(e.root().join("a.txt")).unwrap();
    let v2 = e.map(&[("a.txt", b"A")]);
    assert!(matches!(e.apply(&v1, &v2, &ApplyOpts::default()), Err(ApplyError::LocalChanged(_))));
}

#[test]
fn apply_trusts_content_when_cache_is_cold() {
    let mut e = Env::new();
    let v1 = e.map(&[("a.txt", b"a")]);
    e.apply(&WorkMap::default(), &v1, &ApplyOpts::default()).unwrap();
    e.cache = StatCache::default();
    let v2 = e.map(&[("a.txt", b"A")]);
    e.apply(&v1, &v2, &ApplyOpts::default()).unwrap();
    assert_eq!(e.read("a.txt"), b"A");
}

#[test]
fn apply_removes_empty_dirs_but_keeps_dirs_with_ignored_files() {
    let mut e = Env::new();
    let v1 = e.map(&[("d/a", b"a"), ("x/y/z", b"z")]);
    e.apply(&WorkMap::default(), &v1, &ApplyOpts::default()).unwrap();
    fs::write(e.root().join("d/ignored.log"), b"l").unwrap();
    e.apply(&v1, &WorkMap::default(), &ApplyOpts::default()).unwrap();
    assert!(e.root().join("d/ignored.log").exists());
    assert!(!e.root().join("x").exists());
    assert!(e.root().exists());
}

#[test]
fn apply_skips_case_collision_when_insensitive() {
    let mut e = Env::new();
    let to = e.map(&[("Foo.ts", b"1"), ("foo.ts", b"2")]);
    let r = e.apply(&WorkMap::default(), &to, &ApplyOpts { case_insensitive: true, ..Default::default() }).unwrap();
    assert_eq!(r.written, vec![RelPath::new("Foo.ts")]);
    assert_eq!(r.skipped, vec![(RelPath::new("foo.ts"), SkipReason::CaseCollision(RelPath::new("Foo.ts")))]);
}

#[test]
fn apply_skips_windows_reserved() {
    let mut e = Env::new();
    let to = e.map(&[("aux.js", b"1"), ("ok.js", b"2")]);
    let r = e.apply(&WorkMap::default(), &to, &ApplyOpts { windows_names: true, ..Default::default() }).unwrap();
    assert_eq!(r.written, vec![RelPath::new("ok.js")]);
    assert_eq!(r.skipped, vec![(RelPath::new("aux.js"), SkipReason::WindowsName(NameProblem::ReservedName("aux.js".into())))]);
    assert!(!SkipReason::CaseCollision(RelPath::new("a")).to_string().is_empty());
}

#[test]
fn apply_updates_statcache_so_rescan_sees_no_change() {
    let mut e = Env::new();
    let to = e.map(&[("a.txt", b"a"), ("b/c.txt", b"c")]);
    e.apply(&WorkMap::default(), &to, &ApplyOpts::default()).unwrap();
    e.cache.written_at_ns = i64::MAX;
    let m = Matcher::new(&e.root(), &ProjectConfig::default()).unwrap();
    let dirty = [RelPath::new("a.txt"), RelPath::new("b")].into_iter().collect();
    let r = incremental_scan(&e.root(), &m, &e.store, &mut e.cache, &to, &dirty).unwrap();
    assert_eq!(r.work, to);
    assert_eq!(r.stats.hashed, 0);
    let full = full_scan(&e.root(), &m, &e.store, &mut e.cache, &to).unwrap();
    assert_eq!(full.work, to);
}

#[test]
fn file_replaced_by_dir_and_back() {
    let mut e = Env::new();
    let v1 = e.map(&[("a", b"file")]);
    e.apply(&WorkMap::default(), &v1, &ApplyOpts::default()).unwrap();
    let v2 = e.map(&[("a/b", b"nested")]);
    e.apply(&v1, &v2, &ApplyOpts::default()).unwrap();
    assert_eq!(e.read("a/b"), b"nested");
    e.apply(&v2, &v1, &ApplyOpts::default()).unwrap();
    assert_eq!(e.read("a"), b"file");
}

struct FailAfter(usize);
impl ApplyFaults for FailAfter {
    fn before_write(&self, index: usize, _rel: &RelPath) -> io::Result<()> {
        if index >= self.0 { Err(io::Error::other("injected crash")) } else { Ok(()) }
    }
}

#[test]
fn injected_fault_stops_mid_apply_without_tmp_leftovers() {
    let mut e = Env::new();
    let to = e.map(&[("a", b"1"), ("b", b"2"), ("c", b"3")]);
    let faults = FailAfter(1);
    let err = e.apply(&WorkMap::default(), &to, &ApplyOpts { faults: Some(&faults), ..Default::default() }).unwrap_err();
    assert!(matches!(err, ApplyError::Core(_)), "{err}");
    let names: Vec<String> = fs::read_dir(e.root()).unwrap().map(|d| d.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, vec!["a"]);
}

#[test]
fn gitlinks_are_not_checked_out() {
    let mut e = Env::new();
    let mut to = e.map(&[("a", b"1")]);
    let t = e.store.build_tree(&WorkMap::default()).unwrap();
    let c = e.store.commit(t, &[], "x", 0).unwrap();
    to.insert(RelPath::new("sub"), Entry { oid: c, mode: EntryMode::Commit });
    let r = e.apply(&WorkMap::default(), &to, &ApplyOpts::default()).unwrap();
    assert_eq!(r.written, vec![RelPath::new("a")]);
}

#[cfg(unix)]
#[test]
fn apply_exec_and_symlink() {
    use std::os::unix::fs::PermissionsExt;
    let mut e = Env::new();
    let mut to = WorkMap::default();
    to.insert(RelPath::new("run.sh"), Entry { oid: e.store.write_blob(b"#!/bin/sh").unwrap(), mode: EntryMode::BlobExec });
    to.insert(RelPath::new("link"), Entry { oid: e.store.write_blob(b"run.sh").unwrap(), mode: EntryMode::Link });
    e.apply(&WorkMap::default(), &to, &ApplyOpts::default()).unwrap();
    assert_eq!(fs::metadata(e.root().join("run.sh")).unwrap().permissions().mode() & 0o111, 0o111);
    assert_eq!(fs::read_link(e.root().join("link")).unwrap(), std::path::PathBuf::from("run.sh"));
    let m = Matcher::new(&e.root(), &ProjectConfig::default()).unwrap();
    e.cache.written_at_ns = i64::MAX;
    let r = full_scan(&e.root(), &m, &e.store, &mut e.cache, &to).unwrap();
    assert_eq!(r.work, to);
    // mode-only change is applied
    let mut to2 = to.clone();
    to2.get_mut(&RelPath::new("run.sh")).unwrap().mode = EntryMode::Blob;
    e.apply(&to, &to2, &ApplyOpts::default()).unwrap();
    assert_eq!(fs::metadata(e.root().join("run.sh")).unwrap().permissions().mode() & 0o111, 0);
}

#[test]
fn untracked_dir_in_the_way_is_never_deleted() {
    let mut e = Env::new();
    std::fs::create_dir_all(e.root().join("build/out")).unwrap();
    std::fs::write(e.root().join("build/out/precious.bin"), b"user data").unwrap();
    let to = e.map(&[("build", b"now a file remotely"), ("ok", b"1")]);
    let r = e.apply(&WorkMap::default(), &to, &ApplyOpts::default()).unwrap();
    assert!(e.root().join("build/out/precious.bin").exists());
    assert_eq!(r.skipped, vec![(RelPath::new("build"), SkipReason::DirInTheWay)]);
    assert_eq!(r.written, vec![RelPath::new("ok")]);
}
