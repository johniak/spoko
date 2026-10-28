use std::fs;

use spoko_core::config::ProjectConfig;
use spoko_core::filter::{Class, Matcher};
use spoko_core::platform::RelPath;

fn setup(gitignore: &str, cfg: &str) -> (tempfile::TempDir, Matcher) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".gitignore"), gitignore).unwrap();
    fs::write(dir.path().join(".spokoconfig"), cfg).unwrap();
    let cfg = ProjectConfig::load(dir.path()).unwrap();
    let m = Matcher::new(dir.path(), &cfg).unwrap();
    (dir, m)
}

fn c(m: &Matcher, p: &str, dir: bool) -> Class {
    m.classify(&RelPath::new(p), dir)
}

#[test]
fn gitignore_inherited() {
    let (_d, m) = setup("node_modules/\n*.log\n", "");
    assert_eq!(c(&m, "node_modules", true), Class::Ignored);
    assert_eq!(c(&m, "node_modules/x/index.js", false), Class::Ignored);
    assert_eq!(c(&m, "app.log", false), Class::Ignored);
    assert_eq!(c(&m, "src/main.rs", false), Class::Synced);
    assert!(!m.should_descend(&RelPath::new("node_modules")));
    assert!(m.should_descend(&RelPath::new("src")));
}

#[test]
fn include_overrides_gitignore() {
    let (_d, m) = setup(".env\n.env.local\n", "[ignore]\ninclude = [\".env*\"]\n");
    assert_eq!(c(&m, ".env", false), Class::Synced);
    assert_eq!(c(&m, "apps/web/.env.local", false), Class::Synced);
}

#[test]
fn exclude_beats_include() {
    let (_d, m) = setup("", "[ignore]\ninclude = [\"*.env\"]\nexclude = [\"secret.env\", \"tmp/\"]\n");
    assert_eq!(c(&m, "secret.env", false), Class::Ignored);
    assert_eq!(c(&m, "a.env", false), Class::Synced);
    assert_eq!(c(&m, "tmp", true), Class::Ignored);
    assert_eq!(c(&m, "tmp/x", false), Class::Ignored);
}

#[test]
fn inherit_false_ignores_gitignore() {
    let (_d, m) = setup("*.log\n", "[ignore]\ninherit_gitignore = false\n");
    assert_eq!(c(&m, "app.log", false), Class::Synced);
}

#[test]
fn always_ignored_ds_store_editor_tmp_and_git() {
    let (_d, m) = setup("", "");
    for p in [".DS_Store", "a/Thumbs.db", ".idea/workspace.xml", "x/.spoko-tmp-1", "a.swp", "b.swx", "4913", "c.txt~", "d___jb_tmp___", "e___jb_old___"] {
        assert_eq!(c(&m, p, false), Class::Ignored, "{p}");
    }
    assert_eq!(c(&m, ".git", true), Class::Ignored);
    assert_eq!(c(&m, ".git/HEAD", false), Class::Ignored);
    assert_eq!(c(&m, ".idea/misc.xml", false), Class::Synced);
}

#[test]
fn descend_only_when_include_prefix() {
    let (_d, m) = setup("config/\nnode_modules/\n", "[ignore]\ninclude = [\"config/local.*\", \".env*\"]\n");
    assert!(m.should_descend(&RelPath::new("config")));
    assert_eq!(c(&m, "config/local.toml", false), Class::Synced);
    assert_eq!(c(&m, "config/prod.toml", false), Class::Ignored);
    assert!(!m.should_descend(&RelPath::new("node_modules")));
    assert_eq!(c(&m, "node_modules/.env", false), Class::Ignored);
}

#[test]
fn nested_gitignore_applies() {
    let (d, _) = setup("", "");
    fs::create_dir_all(d.path().join("sub")).unwrap();
    fs::write(d.path().join("sub/.gitignore"), "gen/\n!keep.log\n").unwrap();
    fs::write(d.path().join(".gitignore"), "*.log\n").unwrap();
    let m = Matcher::new(d.path(), &ProjectConfig::default()).unwrap();
    assert_eq!(c(&m, "sub/gen", true), Class::Ignored);
    assert_eq!(c(&m, "sub/gen/a.rs", false), Class::Ignored);
    assert_eq!(c(&m, "gen/a.rs", false), Class::Synced);
    assert_eq!(c(&m, "sub/keep.log", false), Class::Synced);
    assert_eq!(c(&m, "sub/other.log", false), Class::Ignored);
}

#[test]
fn git_info_exclude_applies() {
    let (d, _) = setup("", "");
    fs::create_dir_all(d.path().join(".git/info")).unwrap();
    fs::write(d.path().join(".git/info/exclude"), "local-only/\n").unwrap();
    let m = Matcher::new(d.path(), &ProjectConfig::default()).unwrap();
    assert_eq!(c(&m, "local-only/x", false), Class::Ignored);
}

#[test]
fn lfs_attribute_and_patterns_are_binary() {
    let (d, _) = setup("", "[binary]\npatterns = [\"media/**\"]\nsize_threshold = 100\n");
    fs::write(d.path().join(".gitattributes"), "*.psd filter=lfs diff=lfs merge=lfs -text\n# comment\n\n*.txt text\n").unwrap();
    let m = Matcher::new(d.path(), &ProjectConfig::load(d.path()).unwrap()).unwrap();
    assert!(m.is_binary_pattern(&RelPath::new("art/x.psd")));
    assert!(m.is_binary_pattern(&RelPath::new("media/a/b.png")));
    assert!(!m.is_binary_pattern(&RelPath::new("notes.txt")));
    assert!(m.is_binary(&RelPath::new("a.txt"), b"abc\0def", 7));
    assert!(m.is_binary(&RelPath::new("a.txt"), b"abc", 101));
    assert!(!m.is_binary(&RelPath::new("a.txt"), b"abc", 3));
}

#[test]
fn watch_hook_match() {
    let (_d, m) = setup("", "[[watch]]\nfiles = [\"package-lock.json\"]\nrun = \"npm ci\"\n[[watch]]\nfiles = [\"Cargo.lock\", \"rust-toolchain.toml\"]\n");
    assert_eq!(m.watch_hooks_for(&RelPath::new("package-lock.json")), vec![0]);
    assert_eq!(m.watch_hooks_for(&RelPath::new("web/package-lock.json")), vec![0]);
    assert_eq!(m.watch_hooks_for(&RelPath::new("rust-toolchain.toml")), vec![1]);
    assert!(m.watch_hooks_for(&RelPath::new("README.md")).is_empty());
}

#[test]
fn bad_pattern_is_error() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join(".spokoconfig"), "[ignore]\ninclude = [\"a{b\"]\n").unwrap();
    let cfg = ProjectConfig::load(dir.path()).unwrap();
    assert!(Matcher::new(dir.path(), &cfg).is_err());
}
