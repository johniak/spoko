//! `.git/config` travels between machines minus the keys that describe the machine itself.

use std::path::Path;

/// Keys (or `prefix.*` families) that stay machine-local.
pub const MACHINE_LOCAL_KEYS: &[&str] = &[
    "core.filemode",
    "core.symlinks",
    "core.ignorecase",
    "core.precomposeunicode",
    "core.autocrlf",
    "core.worktree",
    "core.fsmonitor",
    "core.sshcommand",
    "core.hookspath",
    "credential.*",
    "gpg.program",
    "gpg.*.program",
    "safe.directory",
];

pub fn is_machine_local(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    MACHINE_LOCAL_KEYS.iter().any(|pat| match pat.split_once('*') {
        Some((pre, post)) => key.starts_with(pre) && key.ends_with(post) && key.len() > pre.len() + post.len() - 1,
        None => key == *pat,
    })
}

fn temp_config(bytes: &[u8]) -> std::io::Result<(tempfile::TempDir, std::path::PathBuf)> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("config");
    std::fs::write(&path, bytes)?;
    Ok((dir, path))
}

fn local_keys(cfg: &git2::Config) -> Result<Vec<(String, String)>, git2::Error> {
    let mut out = Vec::new();
    let mut entries = cfg.entries(None)?;
    while let Some(e) = entries.next() {
        let e = e?;
        if let (Ok(name), Ok(value)) = (e.name(), e.value())
            && is_machine_local(name)
        {
            out.push((name.to_owned(), value.to_owned()));
        }
    }
    Ok(out)
}

/// Config text with machine-local keys removed (what gets captured).
pub fn strip_local(bytes: &[u8]) -> crate::Result<Vec<u8>> {
    let (_dir, path) = temp_config(bytes)?;
    let mut cfg = git2::Config::open(&path)?;
    for (name, _) in local_keys(&cfg)? {
        cfg.remove_multivar(&name, ".*")?;
    }
    drop(cfg);
    Ok(std::fs::read(&path)?)
}

/// Remote config with this machine's local keys (from `local_config_path`, if it exists) put back.
pub fn merge(remote: &[u8], local_config_path: &Path) -> crate::Result<Vec<u8>> {
    let stripped = strip_local(remote)?;
    let (_dir, path) = temp_config(&stripped)?;
    let local = match std::fs::read(local_config_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    let (_ldir, lpath) = temp_config(&local)?;
    let keep = local_keys(&git2::Config::open(&lpath)?)?;
    let mut cfg = git2::Config::open(&path)?;
    for (name, value) in keep {
        cfg.set_multivar(&name, "^$", &value)?;
    }
    drop(cfg);
    Ok(std::fs::read(&path)?)
}
