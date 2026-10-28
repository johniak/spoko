use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use spoko_crypto::ObjectId;
use uuid::Uuid;

/// Opaque encrypted objects on disk: `root/<project>/<id[0..2]>/<id[2..]>`.
#[derive(Clone)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new(root: PathBuf) -> BlobStore {
        BlobStore { root }
    }

    fn path(&self, project: Uuid, id: &ObjectId) -> PathBuf {
        let hex = id.to_string();
        self.root.join(project.to_string()).join(&hex[..2]).join(&hex[2..])
    }

    pub fn put(&self, project: Uuid, id: &ObjectId, bytes: &[u8]) -> io::Result<()> {
        let path = self.path(project, id);
        if path.exists() {
            return Ok(());
        }
        let dir = path.parent().expect("blob path has parent");
        fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".tmp-{}", Uuid::new_v4()));
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_data()?;
        drop(f);
        fs::rename(&tmp, &path)
    }

    pub fn get(&self, project: Uuid, id: &ObjectId) -> io::Result<Option<Vec<u8>>> {
        match fs::read(self.path(project, id)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn exists(&self, project: Uuid, id: &ObjectId) -> bool {
        self.path(project, id).exists()
    }
}
