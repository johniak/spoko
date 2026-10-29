//! Union merge for append-only JSON-lines logs (Claude Code sessions).

use std::collections::HashSet;

fn uuid_of(line: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(line).ok()?;
    v.get("uuid")?.as_str().map(str::to_owned)
}

/// Lines of `ours`, then lines of `theirs` not already present (by `uuid`, else by exact text).
pub fn union(ours: &[u8], theirs: &[u8]) -> Vec<u8> {
    let split = |b: &[u8]| -> Vec<Vec<u8>> { b.split(|c| *c == b'\n').filter(|l| !l.is_empty()).map(<[u8]>::to_vec).collect() };
    let (o, t) = (split(ours), split(theirs));
    let mut uuids: HashSet<String> = HashSet::new();
    let mut texts: HashSet<Vec<u8>> = HashSet::new();
    let mut lines = Vec::new();
    for l in o.into_iter().chain(t) {
        let fresh = match uuid_of(&l) {
            Some(u) => uuids.insert(u),
            None => texts.insert(l.clone()),
        };
        if fresh {
            lines.push(l);
        }
    }
    let mut out = lines.join(&b'\n');
    let trailing = theirs.ends_with(b"\n") || ours.ends_with(b"\n");
    if trailing && !out.is_empty() {
        out.push(b'\n');
    }
    out
}
