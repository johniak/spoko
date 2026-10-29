//! Content rewriting between disk and snapshot (machine-specific paths inside files).

use crate::platform::RelPath;

pub trait ContentTransform: Sync {
    fn applies(&self, rel: &RelPath) -> bool;
    /// Disk bytes → snapshot bytes.
    fn from_disk(&self, bytes: &[u8]) -> Vec<u8>;
    /// Snapshot bytes → disk bytes.
    fn to_disk(&self, bytes: &[u8]) -> Vec<u8>;
}
