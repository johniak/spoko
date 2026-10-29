use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};
use spoko_crypto::ObjectId;
use uuid::Uuid;

/// Opaque encrypted objects. Each upload batch is appended as one pack file
/// (`root/<project>/<uuid>.pack`, one fsync) and indexed in `root/index.db`
/// (`(project, id) → pack, offset, length`). A pack is indexed only after it is durable,
/// so a crash leaves at most an unreferenced pack.
pub struct BlobStore {
    root: PathBuf,
    index: Mutex<Connection>,
}

fn sql(e: rusqlite::Error) -> io::Error {
    io::Error::other(e)
}

impl BlobStore {
    pub fn open(root: &Path) -> io::Result<BlobStore> {
        fs::create_dir_all(root)?;
        let conn = Connection::open(root.join("index.db")).map_err(sql)?;
        conn.pragma_update(None, "journal_mode", "WAL").map_err(sql)?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS objects (project BLOB NOT NULL, id BLOB NOT NULL, pack TEXT NOT NULL, off INTEGER NOT NULL, len INTEGER NOT NULL, PRIMARY KEY (project, id)) WITHOUT ROWID;").map_err(sql)?;
        Ok(BlobStore { root: root.to_path_buf(), index: Mutex::new(conn) })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.index.lock().expect("blob index mutex poisoned")
    }

    /// Ids of `ids` that are not stored (order kept).
    pub fn missing(&self, project: Uuid, ids: &[ObjectId]) -> io::Result<Vec<ObjectId>> {
        let conn = self.conn();
        let mut st = conn.prepare_cached("SELECT 1 FROM objects WHERE project = ?1 AND id = ?2").map_err(sql)?;
        let mut out = Vec::new();
        for id in ids {
            if !st.exists(params![project.as_bytes(), &id.0[..]]).map_err(sql)? {
                out.push(*id);
            }
        }
        Ok(out)
    }

    pub fn exists(&self, project: Uuid, id: &ObjectId) -> bool {
        self.missing(project, std::slice::from_ref(id)).is_ok_and(|m| m.is_empty())
    }

    /// Stores a batch with a single fsync; already stored ids are skipped.
    pub fn put_batch(&self, project: Uuid, items: &[(ObjectId, Vec<u8>)]) -> io::Result<usize> {
        let ids: Vec<ObjectId> = items.iter().map(|(i, _)| *i).collect();
        let missing: std::collections::HashSet<ObjectId> = self.missing(project, &ids)?.into_iter().collect();
        let mut fresh: BTreeMap<ObjectId, &[u8]> = BTreeMap::new();
        for (id, b) in items {
            if missing.contains(id) {
                fresh.entry(*id).or_insert(b);
            }
        }
        if fresh.is_empty() {
            return Ok(0);
        }
        let dir = self.root.join(project.to_string());
        fs::create_dir_all(&dir)?;
        let pack = format!("{}.pack", Uuid::new_v4().simple());
        let mut f = fs::File::create(dir.join(&pack))?;
        let mut rows = Vec::with_capacity(fresh.len());
        let mut off = 0u64;
        for (id, b) in &fresh {
            f.write_all(b)?;
            rows.push((*id, off, b.len() as u64));
            off += b.len() as u64;
        }
        f.sync_data()?;
        drop(f);
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(sql)?;
        {
            let mut st = tx.prepare_cached("INSERT OR IGNORE INTO objects (project, id, pack, off, len) VALUES (?1, ?2, ?3, ?4, ?5)").map_err(sql)?;
            for (id, o, l) in &rows {
                st.execute(params![project.as_bytes(), &id.0[..], pack, o.cast_signed(), l.cast_signed()]).map_err(sql)?;
            }
        }
        tx.commit().map_err(sql)?;
        Ok(rows.len())
    }

    pub fn put(&self, project: Uuid, id: &ObjectId, bytes: &[u8]) -> io::Result<()> {
        self.put_batch(project, &[(*id, bytes.to_vec())]).map(|_| ())
    }

    /// Stored objects among `ids` (absent ones are skipped); each pack is opened once.
    pub fn get_many(&self, project: Uuid, ids: &[ObjectId]) -> io::Result<Vec<(ObjectId, Vec<u8>)>> {
        let mut by_pack: BTreeMap<String, Vec<(ObjectId, u64, u64)>> = BTreeMap::new();
        {
            let conn = self.conn();
            let mut st = conn.prepare_cached("SELECT pack, off, len FROM objects WHERE project = ?1 AND id = ?2").map_err(sql)?;
            for id in ids {
                let row: Option<(String, i64, i64)> = st.query_row(params![project.as_bytes(), &id.0[..]], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().map_err(sql)?;
                if let Some((pack, off, len)) = row {
                    by_pack.entry(pack).or_default().push((*id, off.cast_unsigned(), len.cast_unsigned()));
                }
            }
        }
        let dir = self.root.join(project.to_string());
        let mut found = std::collections::HashMap::new();
        for (pack, entries) in by_pack {
            let mut f = fs::File::open(dir.join(&pack))?;
            for (id, off, len) in entries {
                f.seek(SeekFrom::Start(off))?;
                let mut buf = vec![0u8; usize::try_from(len).map_err(io::Error::other)?];
                f.read_exact(&mut buf)?;
                found.insert(id, buf);
            }
        }
        // keep the request order
        Ok(ids.iter().filter_map(|id| found.remove(id).map(|b| (*id, b))).collect())
    }

    pub fn get(&self, project: Uuid, id: &ObjectId) -> io::Result<Option<Vec<u8>>> {
        Ok(self.get_many(project, std::slice::from_ref(id))?.pop().map(|(_, b)| b))
    }
}
