//! Directories outside the project that belong to it (Claude Code chats, custom presets).

use std::path::{Path, PathBuf};

use crate::config::ProjectConfig;
use crate::platform::RelPath;
use crate::transform::ContentTransform;

pub const ROOT_PLACEHOLDER: &str = "${SPOKO_PROJECT_ROOT}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalDir {
    /// Key in the snapshot: `external/<name>/`.
    pub name: String,
    pub local_dir: PathBuf,
    pub rewrite_paths: bool,
}

/// Environment the external locations depend on (passed explicitly for testability).
#[derive(Debug, Clone)]
pub struct ExternalEnv {
    pub home: PathBuf,
    /// `$CLAUDE_CONFIG_DIR`, if set.
    pub claude_config_dir: Option<PathBuf>,
}

impl ExternalEnv {
    pub fn from_process() -> ExternalEnv {
        ExternalEnv {
            home: crate::config::home_dir().unwrap_or_else(|| PathBuf::from(".")),
            claude_config_dir: std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()).map(PathBuf::from),
        }
    }
}

/// Claude Code's project directory name: every char outside `[A-Za-z0-9]` becomes `-`.
pub fn encode_claude_dir(path: &str) -> String {
    path.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

pub fn resolve_externals(cfg: &ProjectConfig, project_root: &Path, env: &ExternalEnv) -> Vec<ExternalDir> {
    let mut out = Vec::new();
    if cfg.external.claude_code.enabled {
        let base = env.claude_config_dir.clone().unwrap_or_else(|| env.home.join(".claude"));
        out.push(ExternalDir {
            name: "claude".into(),
            local_dir: base.join("projects").join(encode_claude_dir(&project_root.to_string_lossy())),
            rewrite_paths: true,
        });
    }
    for c in &cfg.external.custom {
        out.push(ExternalDir {
            name: c.name.clone(),
            local_dir: PathBuf::from(c.path.replace("{home}", &env.home.to_string_lossy())),
            rewrite_paths: c.rewrite_paths,
        });
    }
    out
}

fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn replace_bounded(content: &[u8], needle: &[u8], with: &[u8], boundary: &dyn Fn(&[u8]) -> bool) -> Vec<u8> {
    if needle.is_empty() {
        return content.to_vec();
    }
    let mut out = Vec::with_capacity(content.len());
    let mut i = 0;
    while i < content.len() {
        if content[i..].starts_with(needle) && boundary(&content[i + needle.len()..]) {
            out.extend_from_slice(with);
            i += needle.len();
        } else {
            out.push(content[i]);
            i += 1;
        }
    }
    out
}

fn path_boundary(rest: &[u8]) -> bool {
    rest.is_empty() || rest[0] == b'"' || rest[0] == b'/' || rest.starts_with(b"\\\\")
}

/// Replaces the (JSON-escaped) project root with [`ROOT_PLACEHOLDER`] where it starts a path.
pub fn to_neutral(content: &[u8], project_root: &str) -> Vec<u8> {
    replace_bounded(content, json_escape(project_root).as_bytes(), ROOT_PLACEHOLDER.as_bytes(), &path_boundary)
}

pub fn to_local(content: &[u8], project_root: &str) -> Vec<u8> {
    replace_bounded(content, ROOT_PLACEHOLDER.as_bytes(), json_escape(project_root).as_bytes(), &|_| true)
}

/// Rewrites the project root inside `.jsonl` files.
pub struct PathRewrite {
    pub root: String,
}

impl ContentTransform for PathRewrite {
    fn applies(&self, rel: &RelPath) -> bool {
        rel.as_str().ends_with(".jsonl")
    }
    fn disk_to_snapshot(&self, bytes: &[u8]) -> Vec<u8> {
        to_neutral(bytes, &self.root)
    }
    fn snapshot_to_disk(&self, bytes: &[u8]) -> Vec<u8> {
        to_local(bytes, &self.root)
    }
}
