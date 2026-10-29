use std::fs;
use std::path::{Path, PathBuf};

use git2::Oid;
use spoko_core::config::{ProjectConfig, home_dir, spoko_home_from};
use spoko_core::external::{ExternalEnv, to_local, to_neutral};
use spoko_core::filter::Matcher;
use spoko_core::merge::Conflict;
use spoko_core::platform::{RelPath, atomic_write_with};
use spoko_core::snapshot::read_snapshot;
use spoko_core::store::{Entry, EntryMode, Store, WorkMap};

#[test]
fn relpath_from_fs_rejects_parent_components() {
    let root = Path::new("/p");
    assert_eq!(RelPath::from_fs(root, &root.join("a/../b")), None);
    assert_eq!(RelPath::from_fs(root, &root.join("a/b")), Some(RelPath::new("a/b")));
}

#[cfg(unix)]
#[test]
fn read_entry_reports_errors_other_than_missing() {
    let d = tempfile::tempdir().unwrap();
    let too_long = d.path().join("n".repeat(300));
    let err = spoko_core::platform::read_entry(&too_long).unwrap_err();
    assert_ne!(err.kind(), std::io::ErrorKind::NotFound);
    assert_eq!(spoko_core::platform::read_entry(&d.path().join("missing")).unwrap(), None);
}

#[test]
fn failed_atomic_write_leaves_no_file_behind() {
    let d = tempfile::tempdir().unwrap();
    let target = d.path().join("sub/out.txt");
    let err = atomic_write_with(&target, false, &mut |w| {
        w.write_all(b"partial")?;
        Err(std::io::Error::other("source vanished"))
    })
    .unwrap_err();
    assert_eq!(err.to_string(), "source vanished");
    assert!(!target.exists());
    assert_eq!(fs::read_dir(d.path().join("sub")).unwrap().count(), 0, "temp file removed");
}

#[test]
fn matcher_remembers_its_root() {
    let d = tempfile::tempdir().unwrap();
    let m = Matcher::new(d.path(), &ProjectConfig::default()).unwrap();
    assert_eq!(m.root(), d.path());
}

#[test]
fn empty_project_root_rewrites_nothing() {
    let line = br#"{"cwd":"/a/b"}"#;
    assert_eq!(to_neutral(line, ""), line.to_vec());
    assert_eq!(to_local(br#"{"cwd":"${SPOKO_PROJECT_ROOT}"}"#, ""), br#"{"cwd":""}"#.to_vec());
}

/// Runs this test binary again for one test with a changed environment (env vars are process-wide).
fn run_child(test: &str, configure: impl FnOnce(&mut std::process::Command)) {
    let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
    cmd.args(["--exact", test, "--test-threads=1"]).env("SPOKO_COVERAGE_CHILD", "1");
    configure(&mut cmd);
    let out = cmd.output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success() && stdout.contains("1 passed"), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn external_env_without_home_and_with_claude_config_dir() {
    if std::env::var_os("SPOKO_COVERAGE_CHILD").is_some() {
        let env = ExternalEnv::from_process();
        assert_eq!(env.home, PathBuf::from("."));
        assert_eq!(env.claude_config_dir, Some(PathBuf::from("/cfg/claude")));
        return;
    }
    run_child("external_env_without_home_and_with_claude_config_dir", |c| {
        c.env_remove("HOME").env_remove("USERPROFILE").env("CLAUDE_CONFIG_DIR", "/cfg/claude");
    });
}

#[test]
fn external_env_uses_home_and_ignores_empty_claude_config_dir() {
    if std::env::var_os("SPOKO_COVERAGE_CHILD").is_some() {
        let env = ExternalEnv::from_process();
        assert_eq!(env.home, PathBuf::from("/h/me"));
        assert_eq!(home_dir(), Some(PathBuf::from("/h/me")));
        assert_eq!(env.claude_config_dir, None);
        return;
    }
    run_child("external_env_uses_home_and_ignores_empty_claude_config_dir", |c| {
        c.env("HOME", "/h/me").env("USERPROFILE", "/h/me").env("CLAUDE_CONFIG_DIR", "");
    });
}

#[test]
fn spoko_home_without_env_or_home_is_relative() {
    assert_eq!(spoko_home_from(None, None), PathBuf::from("./.spoko"));
    assert_eq!(spoko_home_from(Some("".into()), Some("/h".into())), PathBuf::from("/h/.spoko"));
}

#[test]
fn conflict_oids_serialize_as_hex_and_back() {
    let oid = Oid::from_str("0123456789abcdef0123456789abcdef01234567").unwrap();
    let c = Conflict { path: "work/a.rs".into(), base: Some(oid), theirs: None, theirs_device: "laptop".into(), at: 7 };
    let json = serde_json::to_value(&c).unwrap();
    assert_eq!(json["base"], "0123456789abcdef0123456789abcdef01234567");
    assert!(json["theirs"].is_null());
    assert_eq!(serde_json::from_value::<Conflict>(json).unwrap(), c);
}

#[test]
fn snapshot_without_work_or_conflicts_reads_as_empty() {
    let d = tempfile::tempdir().unwrap();
    let s = Store::open_or_init(&d.path().join("s.git")).unwrap();
    let meta = br#"{"device_id":"00000000-0000-0000-0000-000000000000","device_name":"old","time":5}"#;
    let mut w = WorkMap::default();
    w.insert(RelPath::new("meta/snapshot.json"), Entry { oid: s.write_blob(meta).unwrap(), mode: EntryMode::Blob });
    let commit = s.commit(s.build_tree(&w).unwrap(), &[], "legacy", 5).unwrap();
    let parts = read_snapshot(&s, commit).unwrap();
    assert_eq!(parts.work, s.build_tree(&WorkMap::default()).unwrap());
    assert_eq!(parts.gitstate, None);
    assert!(parts.externals.is_empty());
    assert!(parts.meta.conflicts.is_empty());
    assert_eq!((parts.meta.device_name.as_str(), parts.meta.time, parts.meta.device_id), ("old", 5, uuid::Uuid::nil()));
}

#[test]
fn snapshot_without_meta_is_rejected() {
    let d = tempfile::tempdir().unwrap();
    let s = Store::open_or_init(&d.path().join("s.git")).unwrap();
    let mut w = WorkMap::default();
    w.insert(RelPath::new("work"), Entry { oid: s.build_tree(&WorkMap::default()).unwrap(), mode: EntryMode::Tree });
    let commit = s.commit(s.build_tree(&w).unwrap(), &[], "broken", 5).unwrap();
    match read_snapshot(&s, commit) {
        Err(spoko_core::Error::Other(m)) => assert_eq!(m, "snapshot without meta"),
        other => panic!("{other:?}"),
    }
}
