use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::platform::{DiskEntry, RelPath};
use crate::store::{Entry, EntryMode};

/// Entries whose mtime is this close to the cache's save time are re-hashed (racy-git rule).
pub const RACY_WINDOW_NS: i64 = 2_000_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheEntry {
    pub size: u64,
    pub mtime_ns: i64,
    pub ino: u64,
    pub entry: Entry,
    /// The path holds a placeholder file standing in for a symlink (Windows without privilege).
    pub symlink_placeholder: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StatCache {
    pub entries: BTreeMap<RelPath, CacheEntry>,
    pub written_at_ns: i64,
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

impl StatCache {
    /// A missing or corrupt cache yields an empty one (everything gets re-hashed).
    pub fn load(path: &Path) -> std::io::Result<StatCache> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(bincode::deserialize(&bytes).unwrap_or_default()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(StatCache::default()),
            Err(e) => Err(e),
        }
    }

    pub fn save(&mut self, path: &Path) -> std::io::Result<()> {
        self.written_at_ns = now_ns();
        let bytes = bincode::serialize(self).expect("statcache serializes");
        crate::platform::atomic_write(path, &mut &bytes[..], false)?;
        Ok(())
    }

    pub fn lookup(&self, rel: &RelPath, disk: &DiskEntry) -> Option<Entry> {
        let c = self.entries.get(rel)?;
        let fresh = c.size == disk.size && c.mtime_ns == disk.mtime_ns && c.ino == disk.ino && c.mtime_ns.saturating_add(RACY_WINDOW_NS) < self.written_at_ns;
        let kind_ok = matches!(disk.kind, crate::platform::DiskKind::Symlink(_)) == (c.entry.mode == EntryMode::Link && !c.symlink_placeholder);
        (fresh && kind_ok).then_some(c.entry)
    }

    pub fn record(&mut self, rel: &RelPath, disk: &DiskEntry, entry: Entry) {
        let placeholder = self.entries.get(rel).is_some_and(|c| c.symlink_placeholder);
        self.entries.insert(rel.clone(), CacheEntry { size: disk.size, mtime_ns: disk.mtime_ns, ino: disk.ino, entry, symlink_placeholder: placeholder });
    }

    /// Marks `rel` as a symlink placeholder written by checkout.
    pub fn record_placeholder(&mut self, rel: &RelPath, disk: &DiskEntry, entry: Entry) {
        self.entries.insert(rel.clone(), CacheEntry { size: disk.size, mtime_ns: disk.mtime_ns, ino: disk.ino, entry, symlink_placeholder: true });
    }
}
