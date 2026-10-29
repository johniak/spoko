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

enum Node {
    Leaf(Entry),
    Dir(BTreeMap<String, Node>),
}

fn insert(dir: &mut BTreeMap<String, Node>, comps: &[&str], entry: Entry) {
    if let [name] = comps {
        dir.insert((*name).to_owned(), Node::Leaf(entry));
    } else {
        let child = dir.entry(comps[0].to_owned()).or_insert_with(|| Node::Dir(BTreeMap::new()));
        if let Node::Dir(d) = child {
            insert(d, &comps[1..], entry);
        }
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

/// Canonical git tree bytes; entries sorted as git does (a directory sorts as `name/`).
fn write_dir(write: &mut dyn FnMut(&[u8]) -> Result<Oid, git2::Error>, dir: &BTreeMap<String, Node>) -> Result<Oid, git2::Error> {
    let mut entries: Vec<(Vec<u8>, &str, EntryMode, Oid)> = Vec::with_capacity(dir.len());
    for (name, node) in dir {
        let (mode, oid) = match node {
            Node::Leaf(e) => (e.mode, e.oid),
            Node::Dir(d) => (EntryMode::Tree, write_dir(write, d)?),
        };
        let mut key = name.as_bytes().to_vec();
        if mode == EntryMode::Tree {
            key.push(b'/');
        }
        entries.push((key, name, mode, oid));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut buf = Vec::with_capacity(entries.len() * 40);
    for (_, name, mode, oid) in entries {
        buf.extend_from_slice(mode.octal());
        buf.push(b' ');
        buf.extend_from_slice(name.as_bytes());
        buf.push(0);
        buf.extend_from_slice(oid.as_bytes());
    }
    write(&buf)
}

pub fn build_tree(write: &mut dyn FnMut(&[u8]) -> Result<Oid, git2::Error>, work: &WorkMap) -> Result<Oid, git2::Error> {
    let mut root = BTreeMap::new();
    for (path, entry) in work.iter() {
        insert(&mut root, &path.components().collect::<Vec<_>>(), *entry);
    }
    write_dir(write, &root)
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
