use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

/// Project-relative path: `/`-separated, NFC-normalized, no leading/trailing `/`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RelPath(String);

impl RelPath {
    pub fn new(s: &str) -> RelPath {
        let parts: Vec<String> = s.split('/').filter(|c| !c.is_empty()).map(|c| c.nfc().collect()).collect();
        RelPath(parts.join("/"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `None` when `abs` is not strictly inside `root` or has non-UTF-8 components.
    pub fn from_fs(root: &Path, abs: &Path) -> Option<RelPath> {
        let rel = abs.strip_prefix(root).ok()?;
        let mut parts = Vec::new();
        for c in rel.components() {
            match c {
                Component::Normal(s) => parts.push(s.to_str()?.nfc().collect::<String>()),
                _ => return None,
            }
        }
        if parts.is_empty() { None } else { Some(RelPath(parts.join("/"))) }
    }

    pub fn to_fs(&self, root: &Path) -> PathBuf {
        let mut p = root.to_path_buf();
        p.extend(self.components());
        p
    }

    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().expect("non-empty")
    }

    pub fn parent(&self) -> Option<RelPath> {
        self.0.rfind('/').map(|i| RelPath(self.0[..i].to_owned()))
    }

    pub fn join(&self, name: &str) -> RelPath {
        RelPath::new(&format!("{}/{name}", self.0))
    }

    /// True for `self == dir` or `self` below `dir`.
    pub fn is_within(&self, dir: &RelPath) -> bool {
        self.0 == dir.0 || (self.0.starts_with(&dir.0) && self.0.as_bytes().get(dir.0.len()) == Some(&b'/'))
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NameProblem {
    ReservedName(String),
    InvalidChar(char),
    TrailingDotOrSpace,
}

impl fmt::Display for NameProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NameProblem::ReservedName(n) => write!(f, "nazwa '{n}' jest zarezerwowana na Windows"),
            NameProblem::InvalidChar(c) => write!(f, "znak {c:?} jest niedozwolony na Windows"),
            NameProblem::TrailingDotOrSpace => f.write_str("nazwa kończy się kropką lub spacją (niedozwolone na Windows)"),
        }
    }
}

const RESERVED: &[&str] = &["con", "prn", "aux", "nul"];

fn is_reserved(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or("").to_ascii_lowercase();
    if RESERVED.contains(&stem.as_str()) {
        return true;
    }
    (stem.starts_with("com") || stem.starts_with("lpt")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit() && stem.as_bytes()[3] != b'0'
}

/// First reason a path cannot be created on Windows, if any.
pub fn windows_problem(rel: &RelPath) -> Option<NameProblem> {
    for c in rel.components() {
        if is_reserved(c) {
            return Some(NameProblem::ReservedName(c.to_owned()));
        }
        if let Some(bad) = c.chars().find(|ch| matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\') || (*ch as u32) < 0x20) {
            return Some(NameProblem::InvalidChar(bad));
        }
        if c.ends_with('.') || c.ends_with(' ') {
            return Some(NameProblem::TrailingDotOrSpace);
        }
    }
    None
}
