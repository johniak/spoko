//! Ids known to be on the server (append-only file of 32-byte records).

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use spoko_crypto::ObjectId;

#[derive(Debug, Default)]
pub struct KnownSet {
    ids: HashSet<[u8; 32]>,
    path: Option<PathBuf>,
}

impl KnownSet {
    pub fn in_memory() -> KnownSet {
        KnownSet::default()
    }

    pub fn load(path: &Path) -> std::io::Result<KnownSet> {
        let mut ids = HashSet::new();
        match File::open(path) {
            Ok(mut f) => {
                let mut buf = Vec::new();
                f.read_to_end(&mut buf)?;
                for c in buf.chunks_exact(32) {
                    ids.insert(c.try_into().expect("32 bytes"));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        Ok(KnownSet { ids, path: Some(path.to_path_buf()) })
    }

    pub fn contains(&self, id: &ObjectId) -> bool {
        self.ids.contains(&id.0)
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn insert_many(&mut self, ids: impl IntoIterator<Item = ObjectId>) -> std::io::Result<()> {
        let fresh: Vec<[u8; 32]> = ids.into_iter().map(|i| i.0).filter(|i| self.ids.insert(*i)).collect();
        if let (Some(path), false) = (&self.path, fresh.is_empty()) {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut f = OpenOptions::new().create(true).append(true).open(path)?;
            f.write_all(&fresh.concat())?;
            f.sync_data()?;
        }
        Ok(())
    }

    /// Forget everything (e.g. after the server lost data).
    pub fn clear(&mut self) -> std::io::Result<()> {
        self.ids.clear();
        if let Some(p) = &self.path {
            crate::transfer::known::remove_if_exists(p)?;
        }
        Ok(())
    }
}

pub(crate) fn remove_if_exists(p: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}
