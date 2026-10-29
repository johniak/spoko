//! Which objects of a snapshot still have to go to the server.
//!
//! A commit's role comes from how it was reached, never from its content: snapshot roots are
//! uploaded without their parents (older snapshots are already there), commits reached through
//! gitlinks (`gitstate/tips/*`, submodules) bring their whole history.

use std::collections::HashSet;

use git2::{ObjectType, Oid};
use spoko_core::store::Store;
use spoko_crypto::{Keyring, git_object_id};

use super::known::KnownSet;
use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Snapshot,
    UserCommit,
    Tree,
    Blob,
    /// Tag target or anything whose type is only known after reading it.
    Any,
}

/// Dependencies of a raw git object. `bool` = required (missing → integrity error).
pub fn deps(kind: ObjectType, role: Role, data: &[u8]) -> Vec<(Oid, Role, bool)> {
    match kind {
        ObjectType::Commit => {
            let mut out = Vec::new();
            for line in data.split(|b| *b == b'\n') {
                if line.is_empty() {
                    break;
                }
                if let Some(hex) = line.strip_prefix(b"tree ") {
                    out.extend(parse_hex(hex).map(|o| (o, Role::Tree, true)));
                } else if let Some(hex) = line.strip_prefix(b"parent ")
                    && role != Role::Snapshot
                {
                    out.extend(parse_hex(hex).map(|o| (o, Role::UserCommit, false)));
                }
            }
            out
        }
        ObjectType::Tree => tree_entries(data)
            .into_iter()
            .map(|(mode, oid)| match mode {
                0o40000 => (oid, Role::Tree, true),
                0o160000 => (oid, Role::Any, false),
                _ => (oid, Role::Blob, true),
            })
            .collect(),
        ObjectType::Tag => data
            .split(|b| *b == b'\n')
            .find_map(|l| l.strip_prefix(b"object "))
            .and_then(parse_hex)
            .map(|o| vec![(o, Role::Any, false)])
            .unwrap_or_default(),
        _ => vec![],
    }
}

fn parse_hex(hex: &[u8]) -> Option<Oid> {
    Oid::from_str(std::str::from_utf8(hex).ok()?).ok()
}

/// `(mode, oid)` of every entry of a raw tree object.
pub fn tree_entries(mut data: &[u8]) -> Vec<(u32, Oid)> {
    let mut out = Vec::new();
    while let Some(sp) = data.iter().position(|b| *b == b' ') {
        let mode = std::str::from_utf8(&data[..sp])
            .ok()
            .and_then(|m| u32::from_str_radix(m, 8).ok())
            .unwrap_or(0);
        let Some(nul) = data[sp..].iter().position(|b| *b == 0).map(|n| sp + n) else {
            break;
        };
        let Some(raw) = data.get(nul + 1..nul + 21) else {
            break;
        };
        out.extend(Oid::from_bytes(raw).ok().map(|o| (mode, o)));
        data = &data[nul + 21..];
    }
    out
}

/// Objects reachable from `snapshots` that are not in `known`, dependencies first.
pub fn new_objects(
    store: &Store,
    snapshots: &[Oid],
    known: &KnownSet,
    kr: &Keyring,
) -> Result<Vec<(Oid, ObjectType)>> {
    let odb = store.repo().odb()?;
    let mut out = Vec::new();
    let mut seen: HashSet<Oid> = HashSet::new();
    let mut stack: Vec<(Oid, Role, bool)> = snapshots
        .iter()
        .map(|o| (*o, Role::Snapshot, false))
        .collect();
    while let Some((oid, role, expanded)) = stack.pop() {
        if expanded {
            let kind = odb.read_header(oid)?.1;
            out.push((oid, kind));
            continue;
        }
        if !seen.insert(oid) || known.contains(&git_object_id(kr, oid.as_bytes())) {
            continue;
        }
        let Ok(obj) = odb.read(oid) else { continue }; // e.g. submodule commit not in the store
        stack.push((oid, role, true));
        let role = if role == Role::Any && obj.kind() == ObjectType::Commit {
            Role::UserCommit
        } else {
            role
        };
        for (dep, r, _) in deps(obj.kind(), role, obj.data()) {
            stack.push((dep, r, false));
        }
    }
    Ok(out)
}
