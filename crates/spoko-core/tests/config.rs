use spoko_core::config::*;
use uuid::Uuid;

#[test]
fn defaults_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = ProjectConfig::load(dir.path()).unwrap();
    assert!(cfg.ignore.inherit_gitignore);
    assert!(cfg.ignore.include.is_empty());
    assert_eq!(cfg.binary.size_threshold, ByteSize(10 * 1000 * 1000));
    assert!(cfg.external.claude_code.enabled);
    assert!(cfg.watch.is_empty());
    assert!(cfg.docker.volume.is_empty());
}

#[test]
fn parses_spec_example() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(".spokoconfig"),
        r#"
[ignore]
inherit_gitignore = true
include = [".env*", "config/local.*", "docker-compose.override.yml", ".claude/settings.local.json"]
exclude = ["*.log", "tmp/"]

[binary]
patterns = ["media/**", "uploads/**", "*.psd"]
size_threshold = "10MB"

[[watch]]
files = ["package-lock.json"]
run = "npm ci"
mode = "ask"
run_windows = "npm.cmd ci"

[[watch]]
files = ["Cargo.lock"]
run = "cargo fetch"
mode = "notify"

[[docker.volume]]
name = "pg_data"
mode = "raw"
stop_container = true

[[docker.volume]]
name = "mysql"
mode = "dump"
dump = "mysqldump app"
restore = "mysql app"
on_mismatch = "dump"

[external.claude_code]
enabled = false

[[external.custom]]
name = "codex"
path = "{home}/.codex/sessions"
rewrite_paths = true
"#,
    )
    .unwrap();
    let cfg = ProjectConfig::load(dir.path()).unwrap();
    assert_eq!(cfg.ignore.include.len(), 4);
    assert_eq!(cfg.ignore.exclude, vec!["*.log", "tmp/"]);
    assert_eq!(cfg.binary.patterns[2], "*.psd");
    assert_eq!(cfg.watch[0].mode, HookMode::Ask);
    assert_eq!(cfg.watch[0].run_windows.as_deref(), Some("npm.cmd ci"));
    assert_eq!(cfg.watch[1].mode, HookMode::Notify);
    assert_eq!(cfg.docker.volume[0].mode, VolumeMode::Raw);
    assert!(cfg.docker.volume[0].stop_container);
    assert_eq!(cfg.docker.volume[0].on_mismatch, Mismatch::Warn);
    assert_eq!(cfg.docker.volume[1].mode, VolumeMode::Dump);
    assert_eq!(cfg.docker.volume[1].on_mismatch, Mismatch::Dump);
    assert!(!cfg.external.claude_code.enabled);
    assert_eq!(cfg.external.custom[0].path, "{home}/.codex/sessions");
}

#[test]
fn unknown_key_is_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".spokoconfig"), "[ignore]\nincludez = []\n").unwrap();
    let err = ProjectConfig::load(dir.path()).unwrap_err();
    assert!(err.to_string().contains("line 2"), "{err}");
}

#[test]
fn unreadable_config_is_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".spokoconfig")).unwrap();
    assert!(matches!(ProjectConfig::load(dir.path()), Err(ConfigError::Io(_))));
}

#[test]
fn bytesize_parse_variants() {
    assert_eq!("10MB".parse::<ByteSize>().unwrap(), ByteSize(10_000_000));
    assert_eq!("512KiB".parse::<ByteSize>().unwrap(), ByteSize(512 * 1024));
    assert_eq!("1 GiB".parse::<ByteSize>().unwrap(), ByteSize(1 << 30));
    assert_eq!("2gb".parse::<ByteSize>().unwrap(), ByteSize(2_000_000_000));
    assert_eq!("3kb".parse::<ByteSize>().unwrap(), ByteSize(3000));
    assert_eq!("4MiB".parse::<ByteSize>().unwrap(), ByteSize(4 << 20));
    assert_eq!("123".parse::<ByteSize>().unwrap(), ByteSize(123));
    assert_eq!("7b".parse::<ByteSize>().unwrap(), ByteSize(7));
    assert!("lots".parse::<ByteSize>().is_err());
    assert!("5XB".parse::<ByteSize>().is_err());
    let from_int: BinaryCfg = toml::from_str("size_threshold = 42").unwrap();
    assert_eq!(from_int.size_threshold, ByteSize(42));
    assert!(toml::from_str::<BinaryCfg>("size_threshold = \"x\"").is_err());
    assert!(toml::from_str::<BinaryCfg>("size_threshold = -1").is_err());
    assert!(toml::from_str::<BinaryCfg>("size_threshold = true").is_err());
}

#[test]
fn local_config_roundtrip() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(LocalConfig::load(home.path()).unwrap(), LocalConfig::default());
    let mut cfg = LocalConfig { server_url: Some("http://x".into()), device_name: Some("mac".into()), ..Default::default() };
    let id = Uuid::new_v4();
    cfg.projects.insert(id, LocalProject { path: "/tmp/p".into(), paused: false });
    cfg.save(home.path()).unwrap();
    assert_eq!(LocalConfig::load(home.path()).unwrap(), cfg);
    std::fs::write(home.path().join("config.toml"), "server_url = 1").unwrap();
    assert!(LocalConfig::load(home.path()).is_err());
}

#[test]
fn spoko_home_prefers_env() {
    // SAFETY-free: only this test touches SPOKO_HOME in this process.
    let dir = tempfile::tempdir().unwrap();
    assert!(spoko_home_from(Some(dir.path().as_os_str().to_owned()), None).ends_with(dir.path().file_name().unwrap()));
    assert_eq!(spoko_home_from(None, Some("/home/u".into())), std::path::PathBuf::from("/home/u/.spoko"));
}

#[test]
fn spoko_home_resolves_and_save_reports_errors() {
    assert!(spoko_home().ends_with(".spoko") || std::env::var_os("SPOKO_HOME").is_some());
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-dir");
    std::fs::write(&file, b"x").unwrap();
    assert!(LocalConfig::default().save(&file).is_err());
    std::fs::create_dir(dir.path().join("config.toml")).unwrap();
    assert!(LocalConfig::load(dir.path()).is_err());
    assert!(LocalConfig::default().save(dir.path()).is_err());
}
