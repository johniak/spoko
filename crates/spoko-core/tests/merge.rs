use std::collections::BTreeSet;

use git2::Oid;
use spoko_core::merge::{Conflict, MergeCtx, Side, carry_conflicts, jsonl, merge_trees};
use spoko_core::platform::RelPath;
use spoko_core::store::{Entry, EntryMode, Store, WorkMap};

struct E {
    _d: tempfile::TempDir,
    s: Store,
}

fn env() -> E {
    let d = tempfile::tempdir().unwrap();
    let s = Store::open_or_init(&d.path().join("s.git")).unwrap();
    E { _d: d, s }
}

impl E {
    fn tree(&self, files: &[(&str, &[u8])]) -> Oid {
        let mut w = WorkMap::default();
        for (p, c) in files {
            w.insert(RelPath::new(p), Entry { oid: self.s.write_blob(c).unwrap(), mode: EntryMode::Blob });
        }
        self.s.build_tree(&w).unwrap()
    }
    fn content(&self, tree: Oid, p: &str) -> Option<Vec<u8>> {
        self.s.read_tree(tree).unwrap().get(&RelPath::new(p)).map(|e| self.s.read_blob(e.oid).unwrap())
    }
}

fn ctx<'a>(binary: &'a dyn Fn(&RelPath, &[u8], u64) -> bool, ours_newer: bool, union_jsonl: bool) -> MergeCtx<'a> {
    MergeCtx { prefix: "work/", ours_newer, is_binary: binary, union_jsonl, theirs_device: "linux".into(), now: 42 }
}

fn never(_: &RelPath, head: &[u8], _: u64) -> bool {
    head.contains(&0)
}

#[test]
fn clean_text_merge() {
    let e = env();
    let base = e.tree(&[("a.txt", b"1\n2\n3\n4\n5\n"), ("same", b"s")]);
    let ours = e.tree(&[("a.txt", b"1-ours\n2\n3\n4\n5\n"), ("same", b"s"), ("new-ours", b"o")]);
    let theirs = e.tree(&[("a.txt", b"1\n2\n3\n4\n5-theirs\n"), ("same", b"s"), ("new-theirs", b"t")]);
    let m = merge_trees(&e.s, base, ours, theirs, &ctx(&never, true, false)).unwrap();
    assert_eq!(e.content(m.tree, "a.txt").unwrap(), b"1-ours\n2\n3\n4\n5-theirs\n");
    assert!(e.content(m.tree, "new-ours").is_some() && e.content(m.tree, "new-theirs").is_some());
    assert!(m.new_conflicts.is_empty());
}

#[test]
fn text_conflict_keeps_ours_and_records() {
    let e = env();
    let base = e.tree(&[("a.txt", b"line\n")]);
    let ours = e.tree(&[("a.txt", b"ours\n")]);
    let theirs = e.tree(&[("a.txt", b"theirs\n")]);
    let m = merge_trees(&e.s, base, ours, theirs, &ctx(&never, false, false)).unwrap();
    assert_eq!(e.content(m.tree, "a.txt").unwrap(), b"ours\n");
    assert_eq!(m.new_conflicts.len(), 1);
    let c = &m.new_conflicts[0];
    assert_eq!(c.path, "work/a.txt");
    assert_eq!(e.s.read_blob(c.theirs.unwrap()).unwrap(), b"theirs\n");
    assert_eq!(e.s.read_blob(c.base.unwrap()).unwrap(), b"line\n");
    assert_eq!((c.theirs_device.as_str(), c.at), ("linux", 42));
}

#[test]
fn add_add_conflict_and_clean_add_add() {
    let e = env();
    let base = e.tree(&[]);
    let m = merge_trees(&e.s, base, e.tree(&[("n", b"x\n")]), e.tree(&[("n", b"y\n")]), &ctx(&never, true, false)).unwrap();
    assert_eq!(e.content(m.tree, "n").unwrap(), b"x\n");
    assert_eq!(m.new_conflicts[0].base, None);
    let same = merge_trees(&e.s, base, e.tree(&[("n", b"x\n")]), e.tree(&[("n", b"x\n")]), &ctx(&never, true, false)).unwrap();
    assert!(same.new_conflicts.is_empty());
}

#[test]
fn modify_delete_conflict_keeps_ours() {
    let e = env();
    let base = e.tree(&[("a", b"a"), ("b", b"b")]);
    let ours = e.tree(&[("a", b"a-mod")]);
    let theirs = e.tree(&[("b", b"b-mod")]);
    let m = merge_trees(&e.s, base, ours, theirs, &ctx(&never, true, false)).unwrap();
    assert_eq!(e.content(m.tree, "a").unwrap(), b"a-mod");
    assert_eq!(e.content(m.tree, "b"), None, "ours deleted b");
    assert_eq!(m.new_conflicts.len(), 2);
    let b = m.new_conflicts.iter().find(|c| c.path == "work/b").unwrap();
    assert_eq!(e.s.read_blob(b.theirs.unwrap()).unwrap(), b"b-mod");
    let a = m.new_conflicts.iter().find(|c| c.path == "work/a").unwrap();
    assert_eq!(a.theirs, None);
}

#[test]
fn binary_both_changed_newer_wins() {
    let e = env();
    let base = e.tree(&[("img.png", b"\0base")]);
    let ours = e.tree(&[("img.png", b"\0ours")]);
    let theirs = e.tree(&[("img.png", b"\0theirs")]);
    let m = merge_trees(&e.s, base, ours, theirs, &ctx(&never, true, false)).unwrap();
    assert_eq!(e.content(m.tree, "img.png").unwrap(), b"\0ours");
    assert_eq!(m.binary_overwrites, vec![("work/img.png".to_string(), Side::Theirs)]);
    assert!(m.new_conflicts.is_empty());
    let m = merge_trees(&e.s, base, ours, theirs, &ctx(&never, false, false)).unwrap();
    assert_eq!(e.content(m.tree, "img.png").unwrap(), b"\0theirs");
    assert_eq!(m.binary_overwrites, vec![("work/img.png".to_string(), Side::Ours)]);
}

#[test]
fn binary_pattern_svg_not_textually_merged() {
    let e = env();
    let svg = |p: &RelPath, _: &[u8], _: u64| p.as_str().ends_with(".svg");
    let base = e.tree(&[("i.svg", b"<svg>\n1\n2\n3\n</svg>\n")]);
    let ours = e.tree(&[("i.svg", b"<svg>\n1o\n2\n3\n</svg>\n")]);
    let theirs = e.tree(&[("i.svg", b"<svg>\n1\n2\n3t\n</svg>\n")]);
    let m = merge_trees(&e.s, base, ours, theirs, &ctx(&svg, false, false)).unwrap();
    assert_eq!(e.content(m.tree, "i.svg").unwrap(), b"<svg>\n1\n2\n3t\n</svg>\n");
}

#[test]
fn mode_only_change_takes_changed_side() {
    let e = env();
    let blob = e.s.write_blob(b"#!/bin/sh").unwrap();
    let mk = |mode| {
        let mut w = WorkMap::default();
        w.insert(RelPath::new("run.sh"), Entry { oid: blob, mode });
        e.s.build_tree(&w).unwrap()
    };
    let m = merge_trees(&e.s, mk(EntryMode::Blob), mk(EntryMode::Blob), mk(EntryMode::BlobExec), &ctx(&never, true, false)).unwrap();
    assert_eq!(e.s.read_tree(m.tree).unwrap()[&RelPath::new("run.sh")].mode, EntryMode::BlobExec);
    let both = merge_trees(&e.s, mk(EntryMode::Blob), mk(EntryMode::Link), mk(EntryMode::BlobExec), &ctx(&never, false, false)).unwrap();
    assert_eq!(e.s.read_tree(both.tree).unwrap()[&RelPath::new("run.sh")].mode, EntryMode::BlobExec);
}

#[test]
fn jsonl_union_by_uuid() {
    let ours = b"{\"uuid\":\"1\",\"m\":\"a\"}\n{\"uuid\":\"2\",\"m\":\"b\"}\nplain\n";
    let theirs = b"{\"uuid\":\"1\",\"m\":\"a\"}\n{\"uuid\":\"3\",\"m\":\"c\"}\nplain\nother\n";
    let out = String::from_utf8(jsonl::union(ours, theirs)).unwrap();
    assert_eq!(out, "{\"uuid\":\"1\",\"m\":\"a\"}\n{\"uuid\":\"2\",\"m\":\"b\"}\nplain\n{\"uuid\":\"3\",\"m\":\"c\"}\nother\n");
    assert_eq!(jsonl::union(b"a", b"b"), b"a\nb");
    assert_eq!(jsonl::union(b"", b"x\n"), b"x\n");
}

#[test]
fn jsonl_conflict_resolved_by_union() {
    let e = env();
    let base = e.tree(&[("s.jsonl", b"{\"uuid\":\"1\"}\n")]);
    let ours = e.tree(&[("s.jsonl", b"{\"uuid\":\"1\"}\n{\"uuid\":\"2\"}\n")]);
    let theirs = e.tree(&[("s.jsonl", b"{\"uuid\":\"1\"}\n{\"uuid\":\"3\"}\n")]);
    let m = merge_trees(&e.s, base, ours, theirs, &ctx(&never, true, true)).unwrap();
    assert_eq!(e.content(m.tree, "s.jsonl").unwrap(), b"{\"uuid\":\"1\"}\n{\"uuid\":\"2\"}\n{\"uuid\":\"3\"}\n");
    assert!(m.new_conflicts.is_empty());
}

#[test]
fn carry_conflicts_drops_resolved() {
    let c = |p: &str, at| Conflict { path: p.into(), base: None, theirs: None, theirs_device: "d".into(), at };
    let ours = vec![c("work/a", 1), c("work/b", 1)];
    let theirs = vec![c("work/b", 2), c("work/c", 2)];
    let resolved: BTreeSet<String> = ["work/a".to_string()].into();
    let out = carry_conflicts(&ours, &theirs, &resolved);
    assert_eq!(out.iter().map(|c| (c.path.as_str(), c.at)).collect::<Vec<_>>(), vec![("work/b", 1), ("work/c", 2)]);
    let json = serde_json::to_string(&out[0]).unwrap();
    assert_eq!(serde_json::from_str::<Conflict>(&json).unwrap(), out[0]);
}
