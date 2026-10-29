use std::io::Cursor;

use spoko_core::platform::*;

#[test]
fn relpath_normalizes_and_navigates() {
    let p = RelPath::new("/src//e\u{301}/a.rs/");
    assert_eq!(p.as_str(), "src/é/a.rs");
    assert_eq!(p.file_name(), "a.rs");
    assert_eq!(p.parent().unwrap().as_str(), "src/é");
    assert_eq!(RelPath::new("a").parent(), None);
    assert_eq!(p.components().collect::<Vec<_>>(), vec!["src", "é", "a.rs"]);
    assert_eq!(RelPath::new("src").join("x").as_str(), "src/x");
    assert!(p.is_within(&RelPath::new("src")));
    assert!(p.is_within(&p));
    assert!(!RelPath::new("srcx/a").is_within(&RelPath::new("src")));
    assert_eq!(format!("{p}"), "src/é/a.rs");
}

#[test]
fn relpath_fs_roundtrip() {
    let root = std::path::Path::new("/tmp/proj");
    let p = RelPath::new("a/b.txt");
    let abs = p.to_fs(root);
    assert_eq!(abs, root.join("a").join("b.txt"));
    assert_eq!(RelPath::from_fs(root, &abs), Some(p));
    assert_eq!(RelPath::from_fs(root, std::path::Path::new("/elsewhere/x")), None);
    assert_eq!(RelPath::from_fs(root, root), None);
}

#[test]
fn reserved_names_detected() {
    assert_eq!(windows_problem(&RelPath::new("src/aux.js")), Some(NameProblem::ReservedName("aux.js".into())));
    assert_eq!(windows_problem(&RelPath::new("CON")), Some(NameProblem::ReservedName("CON".into())));
    assert_eq!(windows_problem(&RelPath::new("lpt9.txt")), Some(NameProblem::ReservedName("lpt9.txt".into())));
    assert_eq!(windows_problem(&RelPath::new("COM1")), Some(NameProblem::ReservedName("COM1".into())));
    assert_eq!(windows_problem(&RelPath::new("console.js")), None);
    assert_eq!(windows_problem(&RelPath::new("com10")), None);
    assert_eq!(windows_problem(&RelPath::new("src/ok.rs")), None);
}

#[test]
fn invalid_chars_and_trailing_dot() {
    assert_eq!(windows_problem(&RelPath::new("a:b")), Some(NameProblem::InvalidChar(':')));
    assert_eq!(windows_problem(&RelPath::new("q?")), Some(NameProblem::InvalidChar('?')));
    assert_eq!(windows_problem(&RelPath::new("tab\there")), Some(NameProblem::InvalidChar('\t')));
    assert_eq!(windows_problem(&RelPath::new("dir./x")), Some(NameProblem::TrailingDotOrSpace));
    assert_eq!(windows_problem(&RelPath::new("x ")), Some(NameProblem::TrailingDotOrSpace));
    assert_eq!(NameProblem::InvalidChar(':').to_string(), "znak ':' jest niedozwolony na Windows");
    assert!(NameProblem::ReservedName("CON".into()).to_string().contains("CON"));
    assert!(!NameProblem::TrailingDotOrSpace.to_string().is_empty());
}

#[test]
fn nfc_normalizes_decomposed() {
    assert_eq!(RelPath::new("e\u{301}").as_str(), "é");
    assert_eq!(fold_key(&RelPath::new("E\u{301}.TXT")), "é.txt");
}

#[test]
fn collisions_grouped() {
    let paths = [RelPath::new("Foo.ts"), RelPath::new("foo.ts"), RelPath::new("bar")];
    let groups = find_collisions(paths.iter());
    assert_eq!(groups, vec![vec![RelPath::new("Foo.ts"), RelPath::new("foo.ts")]]);
}

#[test]
fn atomic_write_replaces_and_leaves_no_tmp() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("sub/x.txt");
    let e = atomic_write(&p, &mut Cursor::new(b"one"), false).unwrap();
    assert_eq!(e.size, 3);
    atomic_write(&p, &mut Cursor::new(b"second"), false).unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"second");
    let names: Vec<_> = std::fs::read_dir(dir.path().join("sub")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names.len(), 1);
}

#[cfg(unix)]
#[test]
fn atomic_write_sets_exec_bit() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("run.sh");
    let e = atomic_write(&p, &mut Cursor::new(b"#!/bin/sh"), true).unwrap();
    assert!(e.exec);
    let e = atomic_write(&p, &mut Cursor::new(b"#!/bin/sh"), false).unwrap();
    assert!(!e.exec);
}

#[cfg(unix)]
#[test]
fn read_entry_does_not_follow_symlink() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("t"), b"target").unwrap();
    std::os::unix::fs::symlink("t", dir.path().join("l")).unwrap();
    let e = read_entry(&dir.path().join("l")).unwrap().unwrap();
    assert_eq!(e.kind, DiskKind::Symlink("t".into()));
    let f = read_entry(&dir.path().join("t")).unwrap().unwrap();
    assert_eq!(f.kind, DiskKind::File);
    assert!(f.ino != 0 && f.mtime_ns > 0);
    assert_eq!(read_entry(&dir.path().join("nope")).unwrap(), None);
    assert_eq!(read_entry(dir.path()).unwrap().unwrap().kind, DiskKind::Dir);
}

#[cfg(unix)]
#[test]
fn write_symlink_replaces_existing() {
    let dir = tempfile::tempdir().unwrap();
    let l = dir.path().join("d/link");
    assert_eq!(write_symlink(&l, "a").unwrap(), SymlinkOutcome::Link);
    assert_eq!(write_symlink(&l, "b").unwrap(), SymlinkOutcome::Link);
    assert_eq!(std::fs::read_link(&l).unwrap(), std::path::PathBuf::from("b"));
}

#[test]
fn remove_and_cleanup_tmp() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("a/node_modules")).unwrap();
    std::fs::write(dir.path().join("a/.spoko-tmp-1"), b"").unwrap();
    std::fs::write(dir.path().join("a/node_modules/.spoko-tmp-2"), b"").unwrap();
    std::fs::write(dir.path().join("keep"), b"").unwrap();
    let removed = cleanup_tmp(dir.path(), &|p| p.ends_with("node_modules")).unwrap();
    assert_eq!(removed, 1);
    assert!(!dir.path().join("a/.spoko-tmp-1").exists());
    assert!(dir.path().join("a/node_modules/.spoko-tmp-2").exists());
    remove_file(&dir.path().join("keep")).unwrap();
    remove_file(&dir.path().join("keep")).unwrap();
    assert!(is_tmp_name(".spoko-tmp-x") && !is_tmp_name("x"));
}

#[test]
fn case_probe_matches_fs() {
    let dir = tempfile::tempdir().unwrap();
    let insensitive = fs_is_case_insensitive(dir.path()).unwrap();
    std::fs::write(dir.path().join("UPPER"), b"").unwrap();
    assert_eq!(insensitive, dir.path().join("upper").exists());
    assert!(fs_is_case_insensitive(&dir.path().join("missing")).is_err());
}

#[test]
fn barrier_write_replaces_content() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("state.json");
    spoko_core::platform::atomic_write_barrier(&p, b"one").unwrap();
    spoko_core::platform::atomic_write_barrier(&p, b"two").unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), b"two");
    assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1, "no temp files left");
    assert!(spoko_core::platform::atomic_write_barrier(&d.path().join("missing-dir-file/x").join("y"), b"z").is_ok(), "creates parents");
}
