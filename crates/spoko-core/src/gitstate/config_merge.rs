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

fn all_entries(cfg: &git2::Config) -> Result<Vec<(String, String)>, git2::Error> {
    let mut out = Vec::new();
    let mut entries = cfg.entries(None)?;
    while let Some(e) = entries.next() {
        let e = e?;
        if let (Ok(name), Ok(value)) = (e.name(), e.value()) {
            out.push((name.to_owned(), value.to_owned()));
        }
    }
    Ok(out)
}

fn read_entries(bytes: &[u8]) -> crate::Result<Vec<(String, String)>> {
    let (_dir, path) = temp_config(bytes)?;
    Ok(all_entries(&git2::Config::open(&path)?)?)
}

fn quote(v: &str) -> String {
    let needs = v.is_empty() || v.starts_with(' ') || v.ends_with(' ') || v.contains(['#', ';', '"', '\\', '\n', '\t']);
    if !needs {
        return v.to_owned();
    }
    let esc = v.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\t', "\\t");
    format!("\"{esc}\"")
}

/// Deterministic config text: sections and keys sorted, multivar order kept.
/// Both machines therefore produce byte-identical files for the same settings (no ping-pong).
fn canonical(entries: &[(String, String)]) -> Vec<u8> {
    use std::collections::BTreeMap;
    type Vars<'a> = BTreeMap<String, Vec<&'a str>>;
    let mut sections: BTreeMap<(String, Option<String>), Vars<'_>> = BTreeMap::new();
    for (name, value) in entries {
        let (Some((section, rest)), Some(dot)) = (name.split_once('.'), name.rfind('.')) else { continue };
        let var = &name[dot + 1..];
        let sub = (rest.len() > var.len()).then(|| rest[..rest.len() - var.len() - 1].to_owned());
        sections.entry((section.to_ascii_lowercase(), sub)).or_default().entry(var.to_ascii_lowercase()).or_default().push(value);
    }
    let mut out = String::new();
    for ((section, sub), vars) in sections {
        match sub {
            Some(s) => out.push_str(&format!("[{section} \"{}\"]\n", s.replace('\\', "\\\\").replace('"', "\\\""))),
            None => out.push_str(&format!("[{section}]\n")),
        }
        for (var, values) in vars {
            for v in values {
                out.push_str(&format!("\t{var} = {}\n", quote(v)));
            }
        }
    }
    out.into_bytes()
}

/// Canonical config text with machine-local keys removed (what gets captured).
pub fn strip_local(bytes: &[u8]) -> crate::Result<Vec<u8>> {
    let entries: Vec<_> = read_entries(bytes)?.into_iter().filter(|(k, _)| !is_machine_local(k)).collect();
    Ok(canonical(&entries))
}

/// Remote config with this machine's local keys (from `local_config_path`, if it exists) put back.
pub fn merge(remote: &[u8], local_config_path: &Path) -> crate::Result<Vec<u8>> {
    let mut entries: Vec<_> = read_entries(remote)?.into_iter().filter(|(k, _)| !is_machine_local(k)).collect();
    let local = match std::fs::read(local_config_path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.into()),
    };
    entries.extend(read_entries(&local)?.into_iter().filter(|(k, _)| is_machine_local(k)));
    Ok(canonical(&entries))
}
