use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use spoko_core::config::ProjectConfig;
use spoko_core::filter::Matcher;
use spoko_core::platform::RelPath;
#[cfg(unix)]
use spoko_core::scan::ScanProblem;
use spoko_core::scan::{StatCache, full_scan, incremental_scan};
#[cfg(unix)]
use spoko_core::store::EntryMode;
use spoko_core::store::{Store, WorkMap};

struct Env {
    dir: tempfile::TempDir,
    store: Store,
}

impl Env {
    fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("p")).unwrap();
        let store = Store::open_or_init(&dir.path().join("store.git")).unwrap();
        Env { dir, store }
    }
    fn root(&self) -> std::path::PathBuf {
        self.dir.path().join("p")
    }
    fn write(&self, rel: &str, data: &[u8]) {
        let p = self.root().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, data).unwrap();
    }
    fn matcher(&self) -> Matcher {
        Matcher::new(&self.root(), &ProjectConfig::load(&self.root()).unwrap()).unwrap()
    }
}

fn keys(w: &WorkMap) -> Vec<&str> {
    w.keys().map(RelPath::as_str).collect()
}

fn paths(v: &[&str]) -> BTreeSet<RelPath> {
    v.iter().map(|s| RelPath::new(s)).collect()
}

#[test]
fn full_scan_respects_matcher() {
    let e = Env::new();
    e.write(".gitignore", b"node_modules/\n.env\n");
    e.write(".spokoconfig", b"[ignore]\ninclude = [\".env\"]\n");
    e.write(".env", b"SECRET=1");
    e.write("src/main.rs", b"fn main(){}");
    e.write("node_modules/x/index.js", b"x");
    e.write(".git/HEAD", b"ref");
    let mut cache = StatCache::default();
    let r = full_scan(&e.root(), &e.matcher(), &e.store, &mut cache, &WorkMap::default()).unwrap();
    assert_eq!(keys(&r.work), vec![".env", ".gitignore", ".spokoconfig", "src/main.rs"]);
    assert_eq!(e.store.read_blob(r.work[&RelPath::new(".env")].oid).unwrap(), b"SECRET=1");
    assert!(r.problems.is_empty());
}

#[test]
fn full_scan_does_not_enter_ignored_dirs() {
    let e = Env::new();
    e.write(".gitignore", b"node_modules/\n");
    for i in 0..5 {
        e.write(&format!("node_modules/m{i}/a.js"), b"x");
    }
    e.write("src/a.rs", b"a");
    let r = full_scan(&e.root(), &e.matcher(), &e.store, &mut StatCache::default(), &WorkMap::default()).unwrap();
    assert_eq!(r.stats.dirs_visited, 2); // root + src
}

#[test]
fn incremental_add_modify_delete() {
    let e = Env::new();
    e.write("a.txt", b"a");
    e.write("b.txt", b"b");
    let m = e.matcher();
    let mut cache = StatCache::default();
    let r0 = full_scan(&e.root(), &m, &e.store, &mut cache, &WorkMap::default()).unwrap();
    e.write("a.txt", b"a2");
    fs::remove_file(e.root().join("b.txt")).unwrap();
    e.write("c.txt", b"c");
    let r1 = incremental_scan(&e.root(), &m, &e.store, &mut cache, &r0.work, &paths(&["a.txt", "b.txt", "c.txt", "never-existed"])).unwrap();
    assert_eq!(keys(&r1.work), vec!["a.txt", "c.txt"]);
    assert_eq!(e.store.read_blob(r1.work[&RelPath::new("a.txt")].oid).unwrap(), b"a2");
    assert!(!cache.entries.contains_key(&RelPath::new("b.txt")));
}

#[test]
fn incremental_dir_removed_drops_subtree_and_new_dir_is_scanned() {
    let e = Env::new();
    e.write("d/x", b"x");
    e.write("d/sub/y", b"y");
    e.write("keep", b"k");
    let m = e.matcher();
    let mut cache = StatCache::default();
    let r0 = full_scan(&e.root(), &m, &e.store, &mut cache, &WorkMap::default()).unwrap();
    fs::remove_dir_all(e.root().join("d")).unwrap();
    e.write("n/deep/z", b"z");
    let r1 = incremental_scan(&e.root(), &m, &e.store, &mut cache, &r0.work, &paths(&["d", "n"])).unwrap();
    assert_eq!(keys(&r1.work), vec!["keep", "n/deep/z"]);
}

#[test]
fn incremental_ignores_ignored_and_git_paths() {
    let e = Env::new();
    e.write(".gitignore", b"*.log\n");
    let m = e.matcher();
    let mut cache = StatCache::default();
    let r0 = full_scan(&e.root(), &m, &e.store, &mut cache, &WorkMap::default()).unwrap();
    e.write("x.log", b"l");
    e.write(".git/index", b"i");
    let r1 = incremental_scan(&e.root(), &m, &e.store, &mut cache, &r0.work, &paths(&["x.log", ".git/index", ".git"])).unwrap();
    assert_eq!(keys(&r1.work), vec![".gitignore"]);
}

#[test]
fn incremental_tmp_then_rename_yields_final_file_only() {
    let e = Env::new();
    let m = e.matcher();
    let mut cache = StatCache::default();
    let r0 = full_scan(&e.root(), &m, &e.store, &mut cache, &WorkMap::default()).unwrap();
    e.write("a.txt.swp", b"swap");
    e.write("4913", b"");
    e.write(".spoko-tmp-x", b"");
    e.write("a.txt___jb_tmp___", b"new");
    fs::rename(e.root().join("a.txt___jb_tmp___"), e.root().join("a.txt")).unwrap();
    let dirty = paths(&["a.txt.swp", "4913", ".spoko-tmp-x", "a.txt___jb_tmp___", "a.txt"]);
    let r1 = incremental_scan(&e.root(), &m, &e.store, &mut cache, &r0.work, &dirty).unwrap();
    assert_eq!(keys(&r1.work), vec!["a.txt"]);
}

#[test]
fn cache_hit_skips_hashing() {
    let e = Env::new();
    for i in 0..10 {
        e.write(&format!("f{i}"), b"data");
    }
    let m = e.matcher();
    let mut cache = StatCache::default();
    let r0 = full_scan(&e.root(), &m, &e.store, &mut cache, &WorkMap::default()).unwrap();
    assert_eq!(r0.stats.hashed, 10);
    cache.written_at_ns = i64::MAX; // pretend the cache was saved long after the files' mtimes
    let r1 = full_scan(&e.root(), &m, &e.store, &mut cache, &r0.work).unwrap();
    assert_eq!(r1.stats.hashed, 0);
    assert_eq!(r1.work, r0.work);
}

#[test]
fn racy_entry_rehashed() {
    let e = Env::new();
    e.write("f", b"one");
    let m = e.matcher();
    let mut cache = StatCache::default();
    let r0 = full_scan(&e.root(), &m, &e.store, &mut cache, &WorkMap::default()).unwrap();
    cache.written_at_ns = 0; // cache "older" than the file: entry is racy
    let r1 = full_scan(&e.root(), &m, &e.store, &mut cache, &r0.work).unwrap();
    assert_eq!(r1.stats.hashed, 1);
}

#[test]
fn statcache_persists() {
    let e = Env::new();
    e.write("f", b"one");
    let mut cache = StatCache::default();
    full_scan(&e.root(), &e.matcher(), &e.store, &mut cache, &WorkMap::default()).unwrap();
    let path = e.dir.path().join("statcache.bin");
    cache.save(&path).unwrap();
    let back = StatCache::load(&path).unwrap();
    assert_eq!(back.entries, cache.entries);
    assert!(back.written_at_ns > 0);
    assert_eq!(StatCache::load(&e.dir.path().join("missing")).unwrap().entries.len(), 0);
    fs::write(&path, b"garbage").unwrap();
    assert_eq!(StatCache::load(&path).unwrap().entries.len(), 0, "corrupt cache is discarded, not fatal");
}

#[cfg(unix)]
#[test]
fn symlink_stored_as_link_and_loop_not_followed() {
    let e = Env::new();
    e.write("t", b"target");
    std::os::unix::fs::symlink("t", e.root().join("l")).unwrap();
    std::os::unix::fs::symlink(".", e.root().join("loop")).unwrap();
    let r = full_scan(&e.root(), &e.matcher(), &e.store, &mut StatCache::default(), &WorkMap::default()).unwrap();
    let l = r.work[&RelPath::new("l")];
    assert_eq!(l.mode, EntryMode::Link);
    assert_eq!(e.store.read_blob(l.oid).unwrap(), b"t");
    assert_eq!(r.work[&RelPath::new("loop")].mode, EntryMode::Link);
    assert_eq!(r.work.len(), 3);
}

#[cfg(unix)]
#[test]
fn exec_bit_captured() {
    use std::os::unix::fs::PermissionsExt;
    let e = Env::new();
    e.write("run.sh", b"#!/bin/sh");
    fs::set_permissions(e.root().join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    let r = full_scan(&e.root(), &e.matcher(), &e.store, &mut StatCache::default(), &WorkMap::default()).unwrap();
    assert_eq!(r.work[&RelPath::new("run.sh")].mode, EntryMode::BlobExec);
}

#[cfg(unix)]
#[test]
fn unreadable_file_reported_as_problem() {
    use std::os::unix::fs::PermissionsExt;
    let e = Env::new();
    e.write("secret", b"x");
    fs::set_permissions(e.root().join("secret"), fs::Permissions::from_mode(0o000)).unwrap();
    let r = full_scan(&e.root(), &e.matcher(), &e.store, &mut StatCache::default(), &WorkMap::default()).unwrap();
    fs::set_permissions(e.root().join("secret"), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(r.work.is_empty());
    assert!(matches!(&r.problems[..], [(p, ScanProblem::Unreadable(_))] if p.as_str() == "secret"));
}

#[test]
fn missing_root_is_error() {
    let e = Env::new();
    let m = e.matcher();
    assert!(full_scan(Path::new("/definitely/not/here"), &m, &e.store, &mut StatCache::default(), &WorkMap::default()).is_err());
}
