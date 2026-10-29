use std::collections::BTreeMap;

use git2::Oid;
use spoko_core::gitstate::{GitMergeNote, GitState, Head, StashEntry, device_slug, merge_gitstate};
use spoko_core::store::{Entry, EntryMode};

fn oid(n: u8) -> Oid {
    Oid::from_bytes(&[n; 20]).unwrap()
}

fn gs(refs: &[(&str, u8)]) -> GitState {
    GitState {
        head: Head::Branch("refs/heads/main".into()),
        refs: refs.iter().map(|(k, v)| (k.to_string(), oid(*v))).collect(),
        index_tree: Some(oid(100)),
        stash: vec![],
        files: BTreeMap::new(),
    }
}

#[test]
fn only_theirs_moved() {
    let base = gs(&[("refs/heads/main", 1)]);
    let ours = base.clone();
    let theirs = gs(&[("refs/heads/main", 2), ("refs/heads/new", 3)]);
    let r = merge_gitstate(Some(&base), &ours, &theirs, true, "Linux Box");
    assert_eq!(r.state.refs, theirs.refs);
    assert!(r.notes.is_empty());
}

#[test]
fn both_moved_same_branch_splits() {
    let base = gs(&[("refs/heads/main", 1), ("refs/tags/v1", 1)]);
    let ours = gs(&[("refs/heads/main", 2), ("refs/tags/v1", 4)]);
    let theirs = gs(&[("refs/heads/main", 3), ("refs/tags/v1", 5)]);
    let r = merge_gitstate(Some(&base), &ours, &theirs, true, "Linux Box");
    assert_eq!(r.state.refs["refs/heads/main"], oid(2));
    assert_eq!(r.state.refs["refs/heads/main-linux-box"], oid(3));
    assert_eq!(r.state.refs["refs/tags/v1"], oid(4));
    assert_eq!(r.notes, vec![GitMergeNote::BranchSplit { name: "refs/heads/main".into(), kept: oid(2), split_as: "refs/heads/main-linux-box".into() }]);
    let r2 = merge_gitstate(Some(&base), &ours, &theirs, false, "mac");
    assert_eq!(r2.state.refs["refs/heads/main"], oid(3));
    assert_eq!(r2.state.refs["refs/heads/main-mac"], oid(2));
}

#[test]
fn delete_vs_update_keeps_update() {
    let base = gs(&[("refs/heads/a", 1), ("refs/heads/b", 1)]);
    let ours = gs(&[("refs/heads/a", 2)]);
    let theirs = gs(&[("refs/heads/b", 1)]);
    let r = merge_gitstate(Some(&base), &ours, &theirs, false, "x");
    assert_eq!(r.state.refs["refs/heads/a"], oid(2));
    assert!(!r.state.refs.contains_key("refs/heads/b"), "deleted on ours, untouched on theirs");
}

#[test]
fn stash_union_dedup() {
    let mut ours = gs(&[]);
    ours.stash = vec![StashEntry { oid: oid(1), message: "a".into() }, StashEntry { oid: oid(2), message: "b".into() }];
    let mut theirs = gs(&[]);
    theirs.stash = vec![StashEntry { oid: oid(3), message: "c".into() }, StashEntry { oid: oid(2), message: "b".into() }];
    let r = merge_gitstate(None, &ours, &theirs, false, "x");
    assert_eq!(r.state.stash.iter().map(|s| s.oid).collect::<Vec<_>>(), vec![oid(3), oid(2), oid(1)]);
}

#[test]
fn stash_drop_on_one_side_is_respected() {
    let mut base = gs(&[]);
    base.stash = vec![StashEntry { oid: oid(1), message: "a".into() }];
    let ours = gs(&[]);
    let theirs = base.clone();
    let r = merge_gitstate(Some(&base), &ours, &theirs, true, "x");
    assert!(r.state.stash.is_empty());
}

#[test]
fn head_index_and_files_from_newer() {
    let mut ours = gs(&[]);
    let mut theirs = gs(&[]);
    ours.head = Head::Branch("refs/heads/a".into());
    theirs.head = Head::Branch("refs/heads/b".into());
    theirs.index_tree = Some(oid(9));
    ours.files.insert("config".into(), Entry { oid: oid(7), mode: EntryMode::Blob });
    theirs.files.insert("config".into(), Entry { oid: oid(8), mode: EntryMode::Blob });
    theirs.files.insert("hooks/x".into(), Entry { oid: oid(6), mode: EntryMode::BlobExec });
    let r = merge_gitstate(None, &ours, &theirs, false, "x");
    assert_eq!(r.state.head, Head::Branch("refs/heads/b".into()));
    assert_eq!(r.state.index_tree, Some(oid(9)));
    assert_eq!(r.state.files["config"].oid, oid(8));
    assert!(r.state.files.contains_key("hooks/x"));
    let r = merge_gitstate(None, &ours, &theirs, true, "x");
    assert_eq!(r.state.head, Head::Branch("refs/heads/a".into()));
    assert_eq!(r.state.files["config"].oid, oid(7));
}

#[test]
fn no_base_treats_all_as_added() {
    let ours = gs(&[("refs/heads/a", 1)]);
    let theirs = gs(&[("refs/heads/b", 2), ("refs/heads/a", 3)]);
    let r = merge_gitstate(None, &ours, &theirs, true, "pc");
    assert_eq!(r.state.refs["refs/heads/b"], oid(2));
    assert_eq!(r.state.refs["refs/heads/a"], oid(1));
    assert_eq!(r.state.refs["refs/heads/a-pc"], oid(3));
}

#[test]
fn slug() {
    assert_eq!(device_slug("Jan's MacBook Pro"), "jan-s-macbook-pro");
    assert_eq!(device_slug("--x--"), "x");
    assert_eq!(device_slug("ŁÓŹ"), "device");
}

#[test]
fn scalar_fields_are_three_way_not_newer_wins() {
    let e = |n| Entry { oid: oid(n), mode: EntryMode::Blob };
    let mut base = gs(&[("refs/heads/main", 1), ("refs/heads/dzban", 1)]);
    base.files.insert("config".into(), e(10));
    base.files.insert("hooks/pre-commit".into(), e(11));
    // ours (newer) changed only the index and deleted a hook
    let mut ours = base.clone();
    ours.index_tree = Some(oid(101));
    ours.files.remove("hooks/pre-commit");
    // theirs (older) switched branch and changed config
    let mut theirs = base.clone();
    theirs.head = Head::Branch("refs/heads/dzban".into());
    theirs.files.insert("config".into(), e(12));
    let r = merge_gitstate(Some(&base), &ours, &theirs, true, "b");
    assert_eq!(r.state.head, Head::Branch("refs/heads/dzban".into()), "only theirs moved HEAD");
    assert_eq!(r.state.index_tree, Some(oid(101)), "only ours moved the index");
    assert_eq!(r.state.files.get("config"), Some(&e(12)));
    assert_eq!(r.state.files.get("hooks/pre-commit"), None, "deletion on one side wins over unchanged");
    // both changed HEAD → newer wins
    let mut ours2 = base.clone();
    ours2.head = Head::Detached(oid(1));
    assert_eq!(merge_gitstate(Some(&base), &ours2, &theirs, true, "b").state.head, Head::Detached(oid(1)));
    assert_eq!(merge_gitstate(Some(&base), &ours2, &theirs, false, "b").state.head, theirs.head);
}
