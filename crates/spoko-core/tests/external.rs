use std::fs;
use std::path::{Path, PathBuf};

use spoko_core::checkout::{ApplyOpts, apply_tree};
use spoko_core::config::ProjectConfig;
use spoko_core::external::*;
use spoko_core::filter::Matcher;
use spoko_core::scan::{StatCache, full_scan_with};
use spoko_core::store::{Store, WorkMap};

#[test]
fn encode_macos_path() {
    assert_eq!(encode_claude_dir("/Users/johniak/Projects/spoko"), "-Users-johniak-Projects-spoko");
    assert_eq!(encode_claude_dir("/home/j/my.app_x"), "-home-j-my-app-x");
}

#[test]
fn encode_windows_style_path() {
    assert_eq!(encode_claude_dir(r"C:\Users\x\proj"), "C--Users-x-proj");
}

#[test]
fn neutral_local_roundtrip() {
    let line = br#"{"cwd":"/Users/a/p/src","x":"other","y":"/Users/a/p2/no","z":"/Users/a/p"}"#;
    let neutral = to_neutral(line, "/Users/a/p");
    assert_eq!(
        String::from_utf8(neutral.clone()).unwrap(),
        r#"{"cwd":"${SPOKO_PROJECT_ROOT}/src","x":"other","y":"/Users/a/p2/no","z":"${SPOKO_PROJECT_ROOT}"}"#
    );
    let local = to_local(&neutral, "/home/a/p");
    assert_eq!(String::from_utf8(local).unwrap(), r#"{"cwd":"/home/a/p/src","x":"other","y":"/Users/a/p2/no","z":"/home/a/p"}"#);
}

#[test]
fn windows_paths_are_json_escaped() {
    let line = br#"{"cwd":"C:\\Users\\a\\p","f":"C:\\Users\\a\\p\\src\\x.rs"}"#;
    let neutral = to_neutral(line, r"C:\Users\a\p");
    assert_eq!(String::from_utf8(neutral.clone()).unwrap(), r#"{"cwd":"${SPOKO_PROJECT_ROOT}","f":"${SPOKO_PROJECT_ROOT}\\src\\x.rs"}"#);
    assert_eq!(to_local(&neutral, r"C:\Users\a\p"), line.to_vec());
}

#[test]
fn non_json_lines_and_unrelated_content_untouched() {
    let text = b"plain text mentioning /Users/a/p in prose\n";
    assert_eq!(to_neutral(text, "/elsewhere"), text.to_vec());
    assert_eq!(to_local(b"no placeholder", "/x"), b"no placeholder".to_vec());
}

#[test]
fn claude_config_dir_env_respected() {
    let mut cfg = ProjectConfig::default();
    let env = ExternalEnv { home: PathBuf::from("/home/u"), claude_config_dir: Some(PathBuf::from("/cfg/claude")) };
    let dirs = resolve_externals(&cfg, Path::new("/home/u/proj"), &env);
    assert_eq!(dirs.len(), 1);
    assert_eq!(dirs[0].name, "claude");
    assert_eq!(dirs[0].local_dir, PathBuf::from("/cfg/claude/projects/-home-u-proj"));
    assert!(dirs[0].rewrite_paths);
    let env = ExternalEnv { home: PathBuf::from("/home/u"), claude_config_dir: None };
    assert_eq!(resolve_externals(&cfg, Path::new("/home/u/proj"), &env)[0].local_dir, PathBuf::from("/home/u/.claude/projects/-home-u-proj"));
    cfg.external.claude_code.enabled = false;
    cfg.external.custom.push(spoko_core::config::CustomExternal { name: "codex".into(), path: "{home}/.codex/sessions".into(), rewrite_paths: false });
    let dirs = resolve_externals(&cfg, Path::new("/p"), &env);
    assert_eq!(dirs.len(), 1);
    assert_eq!((dirs[0].name.as_str(), dirs[0].local_dir.clone(), dirs[0].rewrite_paths), ("codex", PathBuf::from("/home/u/.codex/sessions"), false));
}

#[test]
fn scan_and_apply_rewrite_jsonl_between_roots() {
    let d = tempfile::tempdir().unwrap();
    let store = Store::open_or_init(&d.path().join("s.git")).unwrap();
    let a_dir = d.path().join("claude-a");
    fs::create_dir_all(&a_dir).unwrap();
    fs::write(a_dir.join("s1.jsonl"), "{\"cwd\":\"/Users/a/p\"}\n").unwrap();
    fs::write(a_dir.join("memory.md"), "/Users/a/p stays").unwrap();
    let ta = PathRewrite { root: "/Users/a/p".into() };
    let m = Matcher::new(&a_dir, &ProjectConfig::default()).unwrap();
    let mut cache_a = StatCache::default();
    let r = full_scan_with(&a_dir, &m, &store, &mut cache_a, &WorkMap::default(), Some(&ta)).unwrap();
    let blob = r.work[&spoko_core::platform::RelPath::new("s1.jsonl")].oid;
    assert_eq!(store.read_blob(blob).unwrap(), b"{\"cwd\":\"${SPOKO_PROJECT_ROOT}\"}\n");

    let b_dir = d.path().join("claude-b");
    let tb = PathRewrite { root: "/home/b/p".into() };
    let mut cache_b = StatCache::default();
    apply_tree(&b_dir, &store, &mut cache_b, &WorkMap::default(), &r.work, &ApplyOpts { transform: Some(&tb), ..Default::default() }).unwrap();
    assert_eq!(fs::read_to_string(b_dir.join("s1.jsonl")).unwrap(), "{\"cwd\":\"/home/b/p\"}\n");
    assert_eq!(fs::read_to_string(b_dir.join("memory.md")).unwrap(), "/Users/a/p stays");
    // rescan of B yields the same neutral tree (no ping-pong)
    let mb = Matcher::new(&b_dir, &ProjectConfig::default()).unwrap();
    let rb = full_scan_with(&b_dir, &mb, &store, &mut StatCache::default(), &WorkMap::default(), Some(&tb)).unwrap();
    assert_eq!(rb.work, r.work);
    // and a second apply sees the disk as matching
    let mut v2 = r.work.clone();
    v2.remove(&spoko_core::platform::RelPath::new("memory.md"));
    apply_tree(&b_dir, &store, &mut StatCache::default(), &r.work, &v2, &ApplyOpts { transform: Some(&tb), ..Default::default() }).unwrap();
    assert!(!b_dir.join("memory.md").exists());
}
