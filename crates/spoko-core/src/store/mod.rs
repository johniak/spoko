//! spoko's internal bare git repository: object store, snapshots, merge engine.

mod pack;
mod workmap;

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::Path;

use git2::{ObjectType, OdbLookupFlags, Oid, Repository, Signature, Time};

pub use workmap::{Entry, EntryMode, WorkMap};

pub struct Store {
    repo: Repository,
    /// Objects written inside [`Store::batch`], packed at its end.
    pending: RefCell<Option<Pending>>,
    /// Trees known to be stored: rebuilding an unchanged directory skips the odb lookup.
    trees: RefCell<std::collections::HashSet<Oid>>,
    /// Size at which `trees` is forgotten and refilled (bounds a long-running daemon's memory).
    tree_cap: usize,
}

#[derive(Default)]
struct Pending {
    depth: usize,
    objects: HashMap<Oid, (ObjectType, Vec<u8>)>,
    order: Vec<Oid>,
    bytes: usize,
}

/// Objects at most this big are buffered by a batch; bigger ones stream to loose files.
const BATCH_OBJECT_MAX: u64 = 1 << 20;
/// A batch writes an intermediate pack once this much is buffered.
const BATCH_FLUSH_BYTES: usize = 64 << 20;
/// Fewer pending objects than this are written loose (no tiny packs piling up).
const PACK_MIN_OBJECTS: usize = 64;

pub(crate) fn io_err(e: io::Error) -> git2::Error {
    git2::Error::from_str(&e.to_string())
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
        Ok(Store { repo, pending: RefCell::new(None), trees: RefCell::default(), tree_cap: 1 << 20 })
    }

    /// Bounds the set of trees remembered as stored (default 2^20).
    pub fn limit_tree_cache(&mut self, cap: usize) {
        self.tree_cap = cap;
    }

    pub fn repo(&self) -> &Repository {
        &self.repo
    }

    /// Runs `f` with object writes buffered in memory, then stores them as one packfile
    /// (one file + index instead of a loose file per object). Objects written in `f` are
    /// readable inside it. They are stored even if `f` fails. Nested calls join the outer batch.
    pub fn batch<T, E: From<git2::Error>>(&self, f: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        self.pending.borrow_mut().get_or_insert_with(Pending::default).depth += 1;
        let r = f();
        let done = {
            let mut p = self.pending.borrow_mut();
            let p = p.as_mut().expect("batch open");
            p.depth -= 1;
            p.depth == 0
        };
        let flushed = if done {
            let r = self.flush_pending();
            // closed even if writing the pack failed, so later writes are not buffered forever
            self.pending.borrow_mut().take();
            r
        } else {
            Ok(())
        };
        let v = r?;
        flushed?;
        Ok(v)
    }

    fn flush_pending(&self) -> Result<(), git2::Error> {
        let (objects, order) = {
            let mut guard = self.pending.borrow_mut();
            let Some(p) = guard.as_mut() else { return Ok(()) };
            p.bytes = 0;
            (std::mem::take(&mut p.objects), std::mem::take(&mut p.order))
        };
        let odb = self.repo.odb()?;
        if order.len() < PACK_MIN_OBJECTS {
            for oid in &order {
                let (kind, data) = &objects[oid];
                odb.write(*kind, data)?;
            }
        } else {
            let bytes = pack::encode(order.iter().map(|o| {
                let (k, d) = &objects[o];
                (*k, d.as_slice())
            }));
            pack::write(&odb, &bytes)?;
            odb.refresh()?;
        }
        for (oid, (kind, _)) in &objects {
            self.remember_tree(*kind, *oid);
        }
        Ok(())
    }

    fn remember_tree(&self, kind: ObjectType, oid: Oid) {
        if kind == ObjectType::Tree {
            let mut t = self.trees.borrow_mut();
            if t.len() >= self.tree_cap {
                t.clear();
            }
            t.insert(oid);
        }
    }

    fn pending_get<R>(&self, oid: Oid, f: impl FnOnce(ObjectType, &[u8]) -> R) -> Option<R> {
        self.pending.borrow().as_ref().and_then(|p| p.objects.get(&oid)).map(|(k, d)| f(*k, d))
    }

    /// Buffers the object if a batch is open; `Ok(None)` means "no batch, write it yourself".
    fn buffer(&self, kind: ObjectType, data: &[u8]) -> Result<Option<Oid>, git2::Error> {
        let oid = {
            let p = self.pending.borrow();
            // big blobs stay loose: they are read back as streams (upload chunking, checkout)
            let Some(p) = p.as_ref().filter(|_| data.len() as u64 <= BATCH_OBJECT_MAX) else { return Ok(None) };
            let oid = Oid::hash_object(kind, data)?;
            if p.objects.contains_key(&oid) || (kind == ObjectType::Tree && self.trees.borrow().contains(&oid)) {
                return Ok(Some(oid));
            }
            oid
        };
        if self.repo.odb()?.exists_ext(oid, OdbLookupFlags::NO_REFRESH) {
            self.remember_tree(kind, oid);
            return Ok(Some(oid));
        }
        let full = {
            let mut guard = self.pending.borrow_mut();
            let p = guard.as_mut().expect("batch open");
            p.objects.insert(oid, (kind, data.to_vec()));
            p.order.push(oid);
            p.bytes += data.len();
            p.bytes >= BATCH_FLUSH_BYTES
        };
        if full {
            self.flush_pending()?;
        }
        Ok(Some(oid))
    }

    pub fn write_blob(&self, data: &[u8]) -> Result<Oid, git2::Error> {
        match self.buffer(ObjectType::Blob, data)? {
            Some(oid) => Ok(oid),
            None => self.repo.blob(data),
        }
    }

    /// Streams exactly `size` bytes into a new blob without buffering the content.
    pub fn write_blob_stream(&self, reader: &mut dyn Read, size: u64) -> Result<Oid, git2::Error> {
        if size <= BATCH_OBJECT_MAX && self.pending.borrow().is_some() {
            let mut buf = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
            reader.take(size).read_to_end(&mut buf).map_err(io_err)?;
            if buf.len() as u64 != size {
                return Err(git2::Error::from_str("file changed while reading"));
            }
            return self.write_blob(&buf);
        }
        let odb = self.repo.odb()?;
        // a size beyond the address space (32-bit) makes libgit2 refuse the writer
        let mut w = odb.writer(usize::try_from(size).unwrap_or(usize::MAX), ObjectType::Blob)?;
        let copied = io::copy(&mut reader.take(size), &mut w).map_err(io_err)?;
        if copied != size {
            return Err(git2::Error::from_str("file changed while reading"));
        }
        // OdbWriter buffers nothing itself (its flush is a no-op): finalize writes the object
        w.finalize()
    }

    pub fn read_blob(&self, oid: Oid) -> Result<Vec<u8>, git2::Error> {
        if let Some(d) = self.pending_get(oid, |_, d| d.to_vec()) {
            return Ok(d);
        }
        Ok(self.repo.find_blob(oid)?.content().to_vec())
    }

    pub fn blob_size(&self, oid: Oid) -> Result<u64, git2::Error> {
        if let Some(n) = self.pending_get(oid, |_, d| d.len() as u64) {
            return Ok(n);
        }
        Ok(self.repo.odb()?.read_header(oid)?.0 as u64)
    }

    /// Streams a blob into `out`. Big blobs are loose (streamable); packed ones are small.
    pub fn copy_blob_to(&self, oid: Oid, out: &mut dyn Write) -> Result<u64, git2::Error> {
        if let Some(r) = self.pending_get(oid, |_, d| out.write_all(d).map(|()| d.len() as u64)) {
            return r.map_err(io_err);
        }
        let odb = self.repo.odb()?;
        match odb.reader(oid) {
            Ok((mut reader, _, _)) => io::copy(&mut reader, out).map_err(io_err),
            Err(_) => {
                let o = odb.read(oid)?;
                out.write_all(o.data()).map_err(io_err)?;
                Ok(o.data().len() as u64)
            }
        }
    }

    /// Without re-listing `objects/pack` on a miss (libgit2's default): a miss is the common
    /// case while downloading, and this process refreshes after every pack it writes itself.
    pub fn has(&self, oid: Oid) -> bool {
        self.pending_get(oid, |_, _| ()).is_some() || self.repo.odb().is_ok_and(|o| o.exists_ext(oid, OdbLookupFlags::NO_REFRESH))
    }

    pub fn read_raw(&self, oid: Oid) -> Result<Option<(ObjectType, Vec<u8>)>, git2::Error> {
        if let Some(r) = self.pending_get(oid, |k, d| (k, d.to_vec())) {
            return Ok(Some(r));
        }
        let odb = self.repo.odb()?;
        match odb.read(oid) {
            Ok(o) => Ok(Some((o.kind(), o.data().to_vec()))),
            Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn write_raw(&self, kind: ObjectType, data: &[u8]) -> Result<Oid, git2::Error> {
        match self.buffer(kind, data)? {
            Some(oid) => Ok(oid),
            None => self.repo.odb()?.write(kind, data),
        }
    }

    /// Trees are serialized directly (no per-entry existence checks) and written through a
    /// batch: unchanged subtrees cost a hash and a lookup, new ones land in one pack.
    pub fn build_tree(&self, work: &WorkMap) -> Result<Oid, git2::Error> {
        self.batch(|| workmap::build_tree(&mut |bytes| self.write_raw(ObjectType::Tree, bytes), work))
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
