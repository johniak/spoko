//! Decides which project paths are synchronized.
//!
//! Order: `.git` and always-ignored → `exclude` → `include` → inherited gitignore → synced.
//! Slash-less `include` patterns rescue ignored *files* anywhere, but spoko only descends into
//! an ignored *directory* when an `include` pattern names a path inside it (`config/local.*`),
//! so giant trees like `node_modules` are never walked.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::config::ProjectConfig;
use crate::platform::RelPath;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Synced,
    Ignored,
}

pub const ALWAYS_IGNORED: &[&str] = &[
    ".DS_Store",
    "Thumbs.db",
    ".idea/workspace.xml",
    ".spoko-tmp-*",
    "*.swp",
    "*.swx",
    "4913",
    "*~",
    "*___jb_tmp___",
    "*___jb_old___",
];

pub struct Matcher {
    root: PathBuf,
    inherit: bool,
    always: Gitignore,
    exclude: Gitignore,
    include: Gitignore,
    include_dir_prefixes: Vec<String>,
    binary: Gitignore,
    watch: Vec<Gitignore>,
    size_threshold: u64,
    /// Lazily loaded `.gitignore` per directory ("" = root, which also carries `.git/info/exclude`).
    gitignores: Mutex<HashMap<String, Option<Gitignore>>>,
}

fn build(root: &Path, patterns: impl IntoIterator<Item = impl AsRef<str>>) -> Result<Gitignore, ignore::Error> {
    let mut b = GitignoreBuilder::new(root);
    for p in patterns {
        b.add_line(None, p.as_ref())?;
    }
    b.build()
}

fn lfs_patterns(root: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(root.join(".gitattributes")) else { return vec![] };
    text.lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let pattern = parts.next().filter(|p| !p.starts_with('#'))?;
            parts.any(|a| a == "filter=lfs").then(|| pattern.to_owned())
        })
        .collect()
}

/// Literal directory part of an anchored include pattern (`config/local.*` → `config`).
fn literal_dir_prefix(pattern: &str) -> Option<String> {
    let p = pattern.trim_start_matches('/');
    let dir = p.rsplit_once('/')?.0;
    let literal: Vec<&str> = dir.split('/').take_while(|c| !c.contains(['*', '?', '['])).collect();
    (!literal.is_empty()).then(|| literal.join("/"))
}

impl Matcher {
    pub fn new(root: &Path, cfg: &ProjectConfig) -> Result<Matcher, ignore::Error> {
        let mut binary_patterns = cfg.binary.patterns.clone();
        binary_patterns.extend(lfs_patterns(root));
        Ok(Matcher {
            root: root.to_path_buf(),
            inherit: cfg.ignore.inherit_gitignore,
            always: build(root, ALWAYS_IGNORED)?,
            exclude: build(root, &cfg.ignore.exclude)?,
            include: build(root, &cfg.ignore.include)?,
            include_dir_prefixes: cfg.ignore.include.iter().filter_map(|p| literal_dir_prefix(p)).collect(),
            binary: build(root, binary_patterns)?,
            watch: cfg.watch.iter().map(|w| build(root, &w.files)).collect::<Result<_, _>>()?,
            size_threshold: cfg.binary.size_threshold.0,
            gitignores: Mutex::new(HashMap::new()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn matches(gi: &Gitignore, rel: &RelPath, is_dir: bool) -> bool {
        gi.matched_path_or_any_parents(Path::new(rel.as_str()), is_dir).is_ignore()
    }

    fn load_gitignore(&self, dir: &str) -> Option<Gitignore> {
        let base = if dir.is_empty() { self.root.clone() } else { RelPath::new(dir).to_fs(&self.root) };
        let mut b = GitignoreBuilder::new(&base);
        let mut any = false;
        if base.join(".gitignore").is_file() {
            any |= b.add(base.join(".gitignore")).is_none();
        }
        if dir.is_empty() && self.root.join(".git/info/exclude").is_file() {
            any |= b.add(self.root.join(".git/info/exclude")).is_none();
        }
        if any { b.build().ok() } else { None }
    }

    /// Gitignore verdict for one path given the `.gitignore` files of its ancestors (deepest wins).
    fn gitignored_self(&self, rel: &RelPath, is_dir: bool) -> bool {
        let comps: Vec<&str> = rel.components().collect();
        let mut cache = self.gitignores.lock().expect("matcher cache");
        for depth in (0..comps.len()).rev() {
            let dir = comps[..depth].join("/");
            let gi = cache.entry(dir.clone()).or_insert_with(|| self.load_gitignore(&dir));
            if let Some(gi) = gi {
                let sub = comps[depth..].join("/");
                match gi.matched(Path::new(&sub), is_dir) {
                    Match::Ignore(_) => return true,
                    Match::Whitelist(_) => return false,
                    Match::None => {}
                }
            }
        }
        false
    }

    fn gitignored(&self, rel: &RelPath, is_dir: bool) -> bool {
        let comps: Vec<&str> = rel.components().collect();
        (1..comps.len()).any(|n| self.gitignored_self(&RelPath::new(&comps[..n].join("/")), true)) || self.gitignored_self(rel, is_dir)
    }

    pub fn classify(&self, rel: &RelPath, is_dir: bool) -> Class {
        if rel.components().any(|c| c == ".git") || Self::matches(&self.always, rel, is_dir) || Self::matches(&self.exclude, rel, is_dir) {
            return Class::Ignored;
        }
        if self.include_allows(rel, is_dir) {
            return Class::Synced;
        }
        if self.inherit && self.gitignored(rel, is_dir) { Class::Ignored } else { Class::Synced }
    }

    fn include_allows(&self, rel: &RelPath, is_dir: bool) -> bool {
        if !self.include.matched(Path::new(rel.as_str()), is_dir).is_ignore() {
            return false;
        }
        // A slash-less include must not reach inside an ignored directory unless a prefix names it.
        let Some(parent) = rel.parent() else { return true };
        !self.inherit || !self.gitignored(&parent, true) || self.prefix_names(&parent)
    }

    fn prefix_names(&self, dir: &RelPath) -> bool {
        self.include_dir_prefixes.iter().any(|p| RelPath::new(p).is_within(dir) || dir.is_within(&RelPath::new(p)))
    }

    pub fn should_descend(&self, dir: &RelPath) -> bool {
        match self.classify(dir, true) {
            Class::Synced => true,
            Class::Ignored => {
                !dir.components().any(|c| c == ".git")
                    && !Self::matches(&self.always, dir, true)
                    && !Self::matches(&self.exclude, dir, true)
                    && self.prefix_names(dir)
            }
        }
    }

    pub fn is_binary_pattern(&self, rel: &RelPath) -> bool {
        Self::matches(&self.binary, rel, false)
    }

    /// Binary for merge purposes: pattern / LFS, a NUL byte in the first 8000 bytes, or over the size threshold.
    pub fn is_binary(&self, rel: &RelPath, head: &[u8], size: u64) -> bool {
        self.is_binary_pattern(rel) || size > self.size_threshold || head.iter().take(8000).any(|b| *b == 0)
    }

    pub fn watch_hooks_for(&self, rel: &RelPath) -> Vec<usize> {
        self.watch.iter().enumerate().filter(|(_, gi)| Self::matches(gi, rel, false)).map(|(i, _)| i).collect()
    }
}
