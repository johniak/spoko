#![cfg_attr(windows, allow(unused_imports, dead_code))] // helpers of #[cfg(unix)] tests
use std::collections::BTreeSet;
use std::fs;

use spoko_core::config::ProjectConfig;
use spoko_core::external::PathRewrite;
use spoko_core::filter::Matcher;
use spoko_core::platform::{RelPath, read_entry};
#[cfg(unix)]
use spoko_core::scan::ScanProblem;
use spoko_core::scan::{StatCache, full_scan, full_scan_with, incremental_scan};
use spoko_core::store::{Entry, EntryMode, Store, WorkMap};

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

fn dirty(paths: &[&str]) -> BTreeSet<RelPath> {
    paths.iter().map(|p| RelPath::new(p)).collect()
}

/// Restores a directory's permissions when dropped (so the tempdir can be removed).
#[cfg(unix)]
struct Locked(std::path::PathBuf);

#[cfg(unix)]
impl Locked {
    fn new(p: std::path::PathBuf) -> Locked {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o000)).unwrap();
        Locked(p)
    }
}

#[cfg(unix)]
impl Drop for Locked {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

#[test]
fn transformed_scan_of_missing_root_is_an_error() {
    let e = Env::new();
    let t = PathRewrite { root: "/x".into() };
    let r = full_scan_with(&e.root().join("missing"), &e.matcher(), &e.store, &mut StatCache::default(), &WorkMap::default(), Some(&t));
    assert!(matches!(r, Err(spoko_core::Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound));
}

#[test]
fn transformed_scan_skips_ignored_directories_and_files() {
    let e = Env::new();
    e.write(".gitignore", b"build/\n");
    e.write("build/out.o", b"o");
    e.write("src/a.rs", b"a");
    e.write("x.swp", b"swap");
    let t = PathRewrite { root: "/x".into() };
    let r = full_scan_with(&e.root(), &e.matcher(), &e.store, &mut StatCache::default(), &WorkMap::default(), Some(&t)).unwrap();
    assert_eq!(keys(&r.work), [".gitignore", "src/a.rs"]);
    assert!(r.problems.is_empty());
}

#[test]
fn parallel_scan_skips_ignored_files() {
    let e = Env::new();
    e.write("a.txt", b"a");
    e.write("d/.DS_Store", b"junk");
    e.write("d/x.swp", b"swap");
    let r = full_scan(&e.root(), &e.matcher(), &e.store, &mut StatCache::default(), &WorkMap::default()).unwrap();
    assert_eq!(keys(&r.work), ["a.txt"]);
}

#[test]
fn incremental_scan_checks_ancestors_of_nested_dirty_paths() {
    let e = Env::new();
    e.write(".gitignore", b"node_modules/\n");
    e.write("src/lib/a.rs", b"a");
    e.write("node_modules/pkg/i.js", b"i");
    let m = e.matcher();
    let mut cache = StatCache::default();
    let r = incremental_scan(&e.root(), &m, &e.store, &mut cache, &WorkMap::default(), &dirty(&["src/lib/a.rs", "node_modules/pkg/i.js"])).unwrap();
    assert_eq!(keys(&r.work), ["src/lib/a.rs"]);
    assert_eq!(r.stats.hashed, 1);
}

#[test]
fn placeholder_file_standing_in_for_symlink_keeps_link_mode() {
    let e = Env::new();
    e.write("l", b"target");
    e.write("plain", b"target");
    let rel = RelPath::new("l");
    let link = Entry { oid: e.store.write_blob(b"target").unwrap(), mode: EntryMode::Link };
    let mut prev = WorkMap::default();
    prev.insert(rel.clone(), link);
    prev.insert(RelPath::new("plain"), link);
    let mut cache = StatCache::default();
    let disk = read_entry(&e.root().join("l")).unwrap().unwrap();
    cache.record_placeholder(&rel, &disk, link);
    assert!(cache.entries[&rel].symlink_placeholder);
    assert_eq!(cache.lookup(&rel, &disk), None, "never saved: entry is racy, re-hashed");
    let r = full_scan(&e.root(), &e.matcher(), &e.store, &mut cache, &prev).unwrap();
    assert_eq!(r.work[&rel], link);
    assert_eq!(r.work[&RelPath::new("plain")].mode, EntryMode::Blob, "no placeholder marker: a regular file");
    assert!(cache.entries[&rel].symlink_placeholder, "marker survives re-hashing");
}

#[test]
fn statcache_load_of_a_directory_is_an_error() {
    let d = tempfile::tempdir().unwrap();
    assert!(StatCache::load(d.path()).is_err());
}

#[cfg(unix)]
#[test]
fn unreadable_directory_is_reported_by_parallel_scan() {
    let e = Env::new();
    e.write("ok.txt", b"ok");
    e.write("z/locked/secret", b"s");
    e.write("a/locked/secret", b"s");
    let _locks = [Locked::new(e.root().join("z/locked")), Locked::new(e.root().join("a/locked"))];
    let r = full_scan(&e.root(), &e.matcher(), &e.store, &mut StatCache::default(), &WorkMap::default()).unwrap();
    assert_eq!(keys(&r.work), ["ok.txt"]);
    let probs: Vec<&str> = r.problems.iter().map(|(p, k)| {
        assert!(matches!(k, ScanProblem::Unreadable(_)));
        p.as_str()
    }).collect();
    assert_eq!(probs, ["a/locked", "z/locked"], "sorted by path");
}

#[cfg(unix)]
#[test]
fn unreadable_directory_is_reported_by_incremental_scan() {
    let e = Env::new();
    e.write("locked/secret", b"s");
    let _lock = Locked::new(e.root().join("locked"));
    let r = incremental_scan(&e.root(), &e.matcher(), &e.store, &mut StatCache::default(), &WorkMap::default(), &dirty(&["locked"])).unwrap();
    assert!(r.work.is_empty());
    assert!(matches!(&r.problems[..], [(p, ScanProblem::Unreadable(_))] if p.as_str() == "locked"), "{:?}", r.problems);
}

#[cfg(unix)]
#[test]
fn unreadable_file_is_reported_by_transformed_scan() {
    let e = Env::new();
    e.write("ok.txt", b"ok");
    e.write("secret", b"s");
    let _lock = Locked::new(e.root().join("secret"));
    let t = PathRewrite { root: "/x".into() };
    let r = full_scan_with(&e.root(), &e.matcher(), &e.store, &mut StatCache::default(), &WorkMap::default(), Some(&t)).unwrap();
    assert_eq!(keys(&r.work), ["ok.txt"]);
    assert!(matches!(&r.problems[..], [(p, ScanProblem::Unreadable(_))] if p.as_str() == "secret"), "{:?}", r.problems);
}

/// Linux filesystems accept any bytes in names (APFS rejects invalid UTF-8).
#[cfg(target_os = "linux")]
fn non_utf8_env() -> Env {
    use std::os::unix::ffi::OsStrExt;
    let e = Env::new();
    e.write("ok.txt", b"ok");
    fs::create_dir(e.root().join("d")).unwrap();
    for dir in [e.root(), e.root().join("d")] {
        fs::write(dir.join(std::ffi::OsStr::from_bytes(b"bad\xff")), b"x").unwrap();
    }
    e
}

#[cfg(target_os = "linux")]
fn assert_non_utf8_reported(r: &spoko_core::scan::ScanResult) {
    assert_eq!(keys(&r.work), ["ok.txt"]);
    let mut probs: Vec<(&str, &ScanProblem)> = r.problems.iter().map(|(p, k)| (p.as_str(), k)).collect();
    probs.sort_by_key(|p| p.0);
    let lossy = ScanProblem::NonUtf8Name("bad\u{fffd}".into());
    assert_eq!(probs, [("bad\u{fffd}", &lossy), ("d/bad\u{fffd}", &lossy)]);
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_names_are_reported_by_parallel_scan() {
    let e = non_utf8_env();
    let r = full_scan(&e.root(), &e.matcher(), &e.store, &mut StatCache::default(), &WorkMap::default()).unwrap();
    assert_non_utf8_reported(&r);
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_names_are_reported_by_transformed_scan() {
    let e = non_utf8_env();
    let t = PathRewrite { root: "/x".into() };
    let r = full_scan_with(&e.root(), &e.matcher(), &e.store, &mut StatCache::default(), &WorkMap::default(), Some(&t)).unwrap();
    assert_non_utf8_reported(&r);
}
