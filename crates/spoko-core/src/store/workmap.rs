use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};

use git2::{ObjectType, Oid, Repository, TreeWalkMode, TreeWalkResult};

use crate::platform::RelPath;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EntryMode {
    Blob,
    BlobExec,
    Link,
    /// A gitlink (mode 160000) pointing at a commit.
    Commit,
    /// A prebuilt subtree inserted as-is (only used when building trees).
    Tree,
}

impl EntryMode {
    fn from_raw(mode: i32) -> Option<EntryMode> {
        match mode {
            0o100644 | 0o100664 => Some(EntryMode::Blob),
            0o100755 => Some(EntryMode::BlobExec),
            0o120000 => Some(EntryMode::Link),
            0o160000 => Some(EntryMode::Commit),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    #[serde(with = "crate::serde_oid")]
    pub oid: Oid,
    pub mode: EntryMode,
}

/// Flat path → entry map; the in-memory form of a tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkMap(pub BTreeMap<RelPath, Entry>);

impl Deref for WorkMap {
    type Target = BTreeMap<RelPath, Entry>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for WorkMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl EntryMode {
    fn octal(self) -> &'static [u8] {
        match self {
            EntryMode::Blob => b"100644",
            EntryMode::BlobExec => b"100755",
            EntryMode::Link => b"120000",
            EntryMode::Commit => b"160000",
            EntryMode::Tree => b"40000",
        }
    }
}

/// git's `base_name_compare`: a tree compares as if its name ended in `/`.
fn git_order(a: &str, a_tree: bool, b: &str, b_tree: bool) -> std::cmp::Ordering {
    let (x, y) = (a.as_bytes(), b.as_bytes());
    let n = x.len().min(y.len());
    x[..n].cmp(&y[..n]).then_with(|| {
        let next = |s: &[u8], tree: bool| s.get(n).copied().unwrap_or(if tree { b'/' } else { 0 });
        next(x, a_tree).cmp(&next(y, b_tree))
    })
}

type DirEntries<'a> = Vec<(&'a str, EntryMode, Oid)>;

/// Canonical git tree bytes; entries sorted as git does (a directory sorts as `name/`).
fn write_dir(write: &mut dyn FnMut(&[u8]) -> Result<Oid, git2::Error>, mut entries: DirEntries<'_>) -> Result<Oid, git2::Error> {
    entries.sort_by(|a, b| git_order(a.0, a.1 == EntryMode::Tree, b.0, b.1 == EntryMode::Tree));
    let mut buf = Vec::with_capacity(entries.len() * 40);
    for (name, mode, oid) in entries {
        buf.extend_from_slice(mode.octal());
        buf.push(b' ');
        buf.extend_from_slice(name.as_bytes());
        buf.push(0);
        buf.extend_from_slice(oid.as_bytes());
    }
    write(&buf)
}

/// One pass over the path-sorted map: a directory's descendants are contiguous, so open
/// directories form a stack and each is written when the walk leaves it.
pub fn build_tree(write: &mut dyn FnMut(&[u8]) -> Result<Oid, git2::Error>, work: &WorkMap) -> Result<Oid, git2::Error> {
    let mut stack: Vec<(&str, DirEntries<'_>)> = vec![("", Vec::new())];
    for (path, entry) in work.iter() {
        let mut comps: Vec<&str> = path.as_str().split('/').collect();
        let name = comps.pop().expect("non-empty path");
        let common = comps.iter().zip(stack.iter().skip(1)).take_while(|(c, (open, _))| *c == open).count();
        while stack.len() > common + 1 {
            let (dir, entries) = stack.pop().expect("deeper than common");
            let oid = write_dir(write, entries)?;
            stack.last_mut().expect("root").1.push((dir, EntryMode::Tree, oid));
        }
        for c in &comps[common..] {
            stack.push((c, Vec::new()));
        }
        stack.last_mut().expect("root").1.push((name, entry.mode, entry.oid));
    }
    while stack.len() > 1 {
        let (dir, entries) = stack.pop().expect("len > 1");
        let oid = write_dir(write, entries)?;
        stack.last_mut().expect("root").1.push((dir, EntryMode::Tree, oid));
    }
    write_dir(write, stack.pop().expect("root").1)
}

pub fn read_tree(repo: &Repository, tree: Oid) -> Result<WorkMap, git2::Error> {
    let tree = repo.find_tree(tree)?;
    let mut out = WorkMap::default();
    tree.walk(TreeWalkMode::PreOrder, |dir, e| {
        if e.kind() != Some(ObjectType::Tree)
            && let (Ok(name), Some(mode)) = (e.name(), EntryMode::from_raw(e.filemode()))
        {
            out.insert(RelPath::new(&format!("{dir}{name}")), Entry { oid: e.id(), mode });
        }
        TreeWalkResult::Ok
    })?;
    Ok(out)
}
