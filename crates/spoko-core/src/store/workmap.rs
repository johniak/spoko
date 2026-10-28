use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};

use git2::{FileMode, ObjectType, Oid, Repository, TreeWalkMode, TreeWalkResult};

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
    fn file_mode(self) -> FileMode {
        match self {
            EntryMode::Blob => FileMode::Blob,
            EntryMode::BlobExec => FileMode::BlobExecutable,
            EntryMode::Link => FileMode::Link,
            EntryMode::Commit => FileMode::Commit,
            EntryMode::Tree => FileMode::Tree,
        }
    }

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

fn write_dir(repo: &Repository, dir: &BTreeMap<String, Node>) -> Result<Oid, git2::Error> {
    let mut tb = repo.treebuilder(None)?;
    for (name, node) in dir {
        match node {
            Node::Leaf(e) => tb.insert(name, e.oid, e.mode.file_mode().into())?,
            Node::Dir(d) => tb.insert(name, write_dir(repo, d)?, FileMode::Tree.into())?,
        };
    }
    tb.write()
}

pub fn build_tree(repo: &Repository, work: &WorkMap) -> Result<Oid, git2::Error> {
    let mut root = BTreeMap::new();
    for (path, entry) in work.iter() {
        insert(&mut root, &path.components().collect::<Vec<_>>(), *entry);
    }
    write_dir(repo, &root)
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
