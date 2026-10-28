//! spoko's internal bare git repository: object store, snapshots, merge engine.

mod workmap;

use std::io::{self, Read, Write};
use std::path::Path;

use git2::{ObjectType, Oid, Repository, Signature, Time};

pub use workmap::{Entry, EntryMode, WorkMap};

pub struct Store {
    repo: Repository,
}

impl Store {
    pub fn open_or_init(path: &Path) -> Result<Store, git2::Error> {
        let repo = match Repository::open_bare(path) {
            Ok(r) => r,
            Err(_) => {
                let r = Repository::init_bare(path)?;
                let mut cfg = r.config()?;
                cfg.set_bool("core.autocrlf", false)?;
                cfg.set_str("gc.auto", "0")?;
                r
            }
        };
        Ok(Store { repo })
    }

    pub fn repo(&self) -> &Repository {
        &self.repo
    }

    pub fn write_blob(&self, data: &[u8]) -> Result<Oid, git2::Error> {
        self.repo.blob(data)
    }

    /// Streams exactly `size` bytes into a new blob without buffering the content.
    pub fn write_blob_stream(&self, reader: &mut dyn Read, size: u64) -> Result<Oid, git2::Error> {
        let odb = self.repo.odb()?;
        let len = usize::try_from(size).map_err(|_| git2::Error::from_str("blob too large"))?;
        let mut w = odb.writer(len, ObjectType::Blob)?;
        let copied = io::copy(&mut reader.take(size), &mut w).map_err(|e| git2::Error::from_str(&e.to_string()))?;
        if copied != size {
            return Err(git2::Error::from_str("file changed while reading"));
        }
        w.flush().map_err(|e| git2::Error::from_str(&e.to_string()))?;
        w.finalize()
    }

    pub fn read_blob(&self, oid: Oid) -> Result<Vec<u8>, git2::Error> {
        Ok(self.repo.find_blob(oid)?.content().to_vec())
    }

    pub fn blob_size(&self, oid: Oid) -> Result<u64, git2::Error> {
        Ok(self.repo.odb()?.read_header(oid)?.0 as u64)
    }

    /// Streams a blob into `out` (the internal store keeps loose objects, which support streaming).
    pub fn copy_blob_to(&self, oid: Oid, out: &mut dyn Write) -> Result<u64, git2::Error> {
        let odb = self.repo.odb()?;
        let (mut reader, _, _) = odb.reader(oid)?;
        io::copy(&mut reader, out).map_err(|e| git2::Error::from_str(&e.to_string()))
    }

    pub fn has(&self, oid: Oid) -> bool {
        self.repo.odb().is_ok_and(|o| o.exists(oid))
    }

    pub fn read_raw(&self, oid: Oid) -> Result<Option<(ObjectType, Vec<u8>)>, git2::Error> {
        let odb = self.repo.odb()?;
        match odb.read(oid) {
            Ok(o) => Ok(Some((o.kind(), o.data().to_vec()))),
            Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn write_raw(&self, kind: ObjectType, data: &[u8]) -> Result<Oid, git2::Error> {
        self.repo.odb()?.write(kind, data)
    }

    pub fn build_tree(&self, work: &WorkMap) -> Result<Oid, git2::Error> {
        workmap::build_tree(&self.repo, work)
    }

    pub fn read_tree(&self, tree: Oid) -> Result<WorkMap, git2::Error> {
        workmap::read_tree(&self.repo, tree)
    }

    /// Oid of the direct child tree `name`, if it exists and is a tree.
    pub fn subtree(&self, tree: Oid, name: &str) -> Result<Option<Oid>, git2::Error> {
        let t = self.repo.find_tree(tree)?;
        Ok(t.get_name(name).filter(|e| e.kind() == Some(ObjectType::Tree)).map(|e| e.id()))
    }

    pub fn commit(&self, tree: Oid, parents: &[Oid], message: &str, time: i64) -> Result<Oid, git2::Error> {
        let sig = Signature::new("spoko", "spoko@localhost", &Time::new(time, 0))?;
        let tree = self.repo.find_tree(tree)?;
        let parents: Vec<git2::Commit<'_>> = parents.iter().map(|p| self.repo.find_commit(*p)).collect::<Result<_, _>>()?;
        let refs: Vec<&git2::Commit<'_>> = parents.iter().collect();
        self.repo.commit(None, &sig, &sig, message, &tree, &refs)
    }

    pub fn commit_tree(&self, commit: Oid) -> Result<Oid, git2::Error> {
        Ok(self.repo.find_commit(commit)?.tree_id())
    }
}
