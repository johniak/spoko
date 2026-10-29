//! Resolving recorded conflicts with the user's merge tool (or conflict markers).

use std::path::Path;
use std::process::Command;

use git2::{MergeFileInput, MergeFileOptions};
use spoko_core::merge::Conflict;
use spoko_core::platform::{self, RelPath};
use spoko_core::snapshot::read_snapshot;

use crate::engine::Engine;
use crate::{ClientError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeTool {
    /// Shell command; `$BASE $LOCAL $REMOTE $MERGED` are set as in `git mergetool`.
    Command(String),
    /// No tool: write conflict markers, the user edits and runs `spoko resolve --done`.
    Markers,
}

const VSCODE: &str = "code --wait --merge \"$REMOTE\" \"$LOCAL\" \"$BASE\" \"$MERGED\"";

impl MergeTool {
    pub fn from_config(cfg: &git2::Config) -> MergeTool {
        let Ok(tool) = cfg.get_string("merge.tool") else { return MergeTool::Markers };
        if let Ok(cmd) = cfg.get_string(&format!("mergetool.{tool}.cmd")) {
            return MergeTool::Command(cmd);
        }
        match tool.as_str() {
            "vscode" => MergeTool::Command(VSCODE.into()),
            _ => MergeTool::Markers,
        }
    }

    /// The tool git would use in this project (repository config, then global).
    pub fn from_git_config(root: &Path) -> MergeTool {
        let cfg = git2::Repository::open(root).and_then(|r| r.config()).or_else(|_| git2::Config::open_default());
        cfg.map_or(MergeTool::Markers, |c| MergeTool::from_config(&c))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveOutcome {
    Resolved,
    Unchanged,
}

fn key(path: &RelPath) -> String {
    format!("work/{path}")
}

/// Open conflicts of the project (as of the last local snapshot).
pub fn list(e: &Engine) -> Result<Vec<Conflict>> {
    let Some(last) = e.state.last_local else { return Ok(vec![]) };
    Ok(read_snapshot(&e.store, last)?.meta.conflicts.into_iter().filter(|c| !e.state.resolved.contains(&c.path)).collect())
}

/// The conflict is dropped from the project at the next capture.
pub fn mark_resolved(e: &mut Engine, path: &RelPath) -> Result<()> {
    e.state.resolved.insert(key(path));
    e.save()
}

pub fn resolve(e: &mut Engine, path: &RelPath, tool: &MergeTool) -> Result<ResolveOutcome> {
    let c = list(e)?.into_iter().find(|c| c.path == key(path)).ok_or_else(|| ClientError::Other(format!("no conflict at {path}")))?;
    let blob = |o: Option<git2::Oid>| -> Result<Vec<u8>> { Ok(o.map(|o| e.store.read_blob(o)).transpose()?.unwrap_or_default()) };
    let (base, remote) = (blob(c.base)?, blob(c.theirs)?);
    let merged = path.to_fs(&e.root);
    let local = std::fs::read(&merged).unwrap_or_default();
    match tool {
        MergeTool::Markers => {
            let mut opts = MergeFileOptions::new();
            opts.ancestor_label("base").our_label("local").their_label(&c.theirs_device);
            let r = git2::merge_file(&input(&base), &input(&local), &input(&remote), Some(&mut opts))?;
            platform::atomic_write(&merged, &mut r.content(), false)?;
            Ok(ResolveOutcome::Unchanged)
        }
        MergeTool::Command(cmd) => {
            let dir = std::env::temp_dir().join(format!("spoko-resolve-{}", uuid::Uuid::new_v4().simple()));
            std::fs::create_dir_all(&dir)?;
            let name = path.file_name();
            let files = [("BASE", &base), ("LOCAL", &local), ("REMOTE", &remote)];
            let mut vars = vec![("MERGED".to_owned(), merged.to_string_lossy().into_owned())];
            for (var, data) in files {
                let f = dir.join(format!("{var}_{name}"));
                std::fs::write(&f, data)?;
                vars.push((var.to_owned(), f.to_string_lossy().into_owned()));
            }
            let status = shell(cmd, &vars).status();
            let _ = std::fs::remove_dir_all(&dir);
            if !status?.success() {
                return Err(ClientError::Other(format!("merge tool failed: {cmd}")));
            }
            if std::fs::read(&merged).unwrap_or_default() == local {
                return Ok(ResolveOutcome::Unchanged);
            }
            mark_resolved(e, path)?;
            Ok(ResolveOutcome::Resolved)
        }
    }
}

fn input(b: &[u8]) -> MergeFileInput<'_> {
    let mut i = MergeFileInput::new();
    i.content(b);
    i
}

#[cfg(unix)]
fn shell(cmd: &str, vars: &[(String, String)]) -> Command {
    let mut c = Command::new("sh");
    c.arg("-c").arg(cmd).envs(vars.iter().map(|(k, v)| (k, v)));
    c
}

#[cfg(windows)]
fn shell(cmd: &str, vars: &[(String, String)]) -> Command {
    // cmd.exe does not expand $VARS: substitute them textually
    let mut text = cmd.to_owned();
    for (k, v) in vars {
        text = text.replace(&format!("${k}"), v);
    }
    let mut c = Command::new("cmd");
    c.arg("/C").arg(text).envs(vars.iter().map(|(k, v)| (k, v)));
    c
}
