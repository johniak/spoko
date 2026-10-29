//! A snapshot = commit in the internal store whose root tree has
//! `work/`, `gitstate/`, `external/<name>/` and `meta/{snapshot,conflicts}.json`.

use std::collections::BTreeMap;

use git2::Oid;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::merge::Conflict;
use crate::platform::RelPath;
use crate::store::{Entry, EntryMode, Store, WorkMap};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotMeta {
    pub device_id: Uuid,
    pub device_name: String,
    pub time: i64,
    pub conflicts: Vec<Conflict>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotParts {
    pub work: Oid,
    pub gitstate: Option<Oid>,
    pub externals: BTreeMap<String, Oid>,
    pub meta: SnapshotMeta,
}

#[derive(Serialize, Deserialize)]
struct Origin {
    device_id: Uuid,
    device_name: String,
    time: i64,
}

pub fn write_snapshot(store: &Store, parts: &SnapshotParts, parents: &[Oid]) -> crate::Result<Oid> {
    let mut w = WorkMap::default();
    let tree = |oid| Entry { oid, mode: EntryMode::Tree };
    w.insert(RelPath::new("work"), tree(parts.work));
    if let Some(g) = parts.gitstate {
        w.insert(RelPath::new("gitstate"), tree(g));
    }
    for (name, oid) in &parts.externals {
        w.insert(RelPath::new(&format!("external/{name}")), tree(*oid));
    }
    let origin = Origin { device_id: parts.meta.device_id, device_name: parts.meta.device_name.clone(), time: parts.meta.time };
    let blob = |b: Vec<u8>| -> crate::Result<Entry> { Ok(Entry { oid: store.write_blob(&b)?, mode: EntryMode::Blob }) };
    w.insert(RelPath::new("meta/snapshot.json"), blob(serde_json::to_vec(&origin)?)?);
    w.insert(RelPath::new("meta/conflicts.json"), blob(serde_json::to_vec(&parts.meta.conflicts)?)?);
    let root = store.build_tree(&w)?;
    let msg = format!("snapshot from {}", parts.meta.device_name);
    Ok(store.commit(root, parents, &msg, parts.meta.time)?)
}

fn meta_blob(store: &Store, root: Oid, name: &str) -> crate::Result<Option<Vec<u8>>> {
    let Some(meta) = store.subtree(root, "meta")? else { return Ok(None) };
    let t = store.repo().find_tree(meta)?;
    Ok(match t.get_name(name) {
        Some(e) => Some(store.read_blob(e.id())?),
        None => None,
    })
}

pub fn read_snapshot(store: &Store, commit: Oid) -> crate::Result<SnapshotParts> {
    let root = store.commit_tree(commit)?;
    let work = match store.subtree(root, "work")? {
        Some(w) => w,
        None => store.build_tree(&WorkMap::default())?,
    };
    let mut externals = BTreeMap::new();
    if let Some(ext) = store.subtree(root, "external")? {
        for e in store.repo().find_tree(ext)?.iter() {
            if let Ok(name) = e.name() {
                externals.insert(name.to_owned(), e.id());
            }
        }
    }
    let origin: Origin = serde_json::from_slice(&meta_blob(store, root, "snapshot.json")?.ok_or_else(|| crate::Error::Other("snapshot without meta".into()))?)?;
    let conflicts = match meta_blob(store, root, "conflicts.json")? {
        Some(b) => serde_json::from_slice(&b)?,
        None => vec![],
    };
    Ok(SnapshotParts {
        work,
        gitstate: store.subtree(root, "gitstate")?,
        externals,
        meta: SnapshotMeta { device_id: origin.device_id, device_name: origin.device_name, time: origin.time, conflicts },
    })
}

/// Equal content ignoring `meta/snapshot.json` (device, time): no need for a new snapshot.
pub fn same_content(store: &Store, a: Oid, b: Oid) -> crate::Result<bool> {
    let (ra, rb) = (store.commit_tree(a)?, store.commit_tree(b)?);
    let sections = |root: Oid| -> crate::Result<Vec<Option<Oid>>> {
        Ok(vec![store.subtree(root, "work")?, store.subtree(root, "gitstate")?, store.subtree(root, "external")?])
    };
    Ok(sections(ra)? == sections(rb)? && meta_blob(store, ra, "conflicts.json")? == meta_blob(store, rb, "conflicts.json")?)
}
