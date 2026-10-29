//! Content rewriting between disk and snapshot (machine-specific paths inside files).

use crate::platform::RelPath;

pub trait ContentTransform: Sync {
    fn applies(&self, rel: &RelPath) -> bool;
    /// Disk bytes → snapshot bytes.
    fn disk_to_snapshot(&self, bytes: &[u8]) -> Vec<u8>;
    /// Snapshot bytes → disk bytes.
    fn snapshot_to_disk(&self, bytes: &[u8]) -> Vec<u8>;
}
