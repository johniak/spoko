#![allow(dead_code)]
use std::path::Path;

use git2::{Oid, Repository, Signature, Time};

pub fn sig() -> Signature<'static> {
    Signature::new("Jan", "jan@example.com", &Time::new(1_700_000_000, 0)).unwrap()
}

pub fn init_repo(root: &Path) -> Repository {
    let repo = Repository::init(root).unwrap();
    repo.config().unwrap().set_str("user.name", "Jan").unwrap();
    repo.config().unwrap().set_str("user.email", "jan@example.com").unwrap();
    repo
}

/// Writes `files`, stages everything and commits on HEAD.
pub fn commit(repo: &Repository, files: &[(&str, &str)], msg: &str) -> Oid {
    let root = repo.workdir().unwrap();
    for (p, c) in files {
        let abs = root.join(p);
        std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
        std::fs::write(abs, c).unwrap();
    }
    let mut idx = repo.index().unwrap();
    idx.add_all(["*"], git2::IndexAddOption::DEFAULT, None).unwrap();
    idx.write().unwrap();
    let tree = repo.find_tree(idx.write_tree().unwrap()).unwrap();
    let parents: Vec<git2::Commit> = repo.head().ok().and_then(|h| h.target()).map(|o| repo.find_commit(o).unwrap()).into_iter().collect();
    let refs: Vec<&git2::Commit> = parents.iter().collect();
    repo.commit(Some("HEAD"), &sig(), &sig(), msg, &tree, &refs).unwrap()
}

pub fn checkout_new_branch(repo: &Repository, name: &str) {
    let head = repo.head().unwrap().peel_to_commit().unwrap();
    repo.branch(name, &head, false).unwrap();
    repo.set_head(&format!("refs/heads/{name}")).unwrap();
}
