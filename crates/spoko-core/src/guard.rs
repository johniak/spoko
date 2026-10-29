//! Refuse to propagate a sudden mass deletion (unmounted disk, `rm -rf` by accident).

use serde::{Deserialize, Serialize};

use crate::platform::RelPath;
use crate::store::WorkMap;

pub const MASS_DELETE_RATIO: f64 = 0.30;
pub const MASS_DELETE_MIN: usize = 50;
/// Deletions this close together are judged as one: a slow `rm -rf` or `git clean` lands in
/// several watcher batches, each under the threshold on its own.
pub const MASS_DELETE_WINDOW_SECS: i64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardVerdict {
    Ok,
    MassDelete { deleted: usize, total: usize },
}

fn judge(deleted: usize, total: usize) -> GuardVerdict {
    #[allow(clippy::cast_precision_loss)]
    let ratio = if total == 0 { 0.0 } else { deleted as f64 / total as f64 };
    if deleted > MASS_DELETE_MIN && ratio > MASS_DELETE_RATIO { GuardVerdict::MassDelete { deleted, total } } else { GuardVerdict::Ok }
}

pub fn check_deletions(prev: &WorkMap, next: &WorkMap) -> GuardVerdict {
    judge(prev.keys().filter(|p| !next.contains_key(*p)).count(), prev.len())
}

/// A stored capture that deleted paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deletion {
    pub time: i64,
    /// Paths tracked on either side of that capture.
    pub total: usize,
    /// Sorted; paths that came back since are dropped.
    pub paths: Vec<RelPath>,
}

/// Local deletions of the last [`MASS_DELETE_WINDOW_SECS`], oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeleteLog(Vec<Deletion>);

impl DeleteLog {
    /// Judges the capture `prev` → `next` at `now` on its own and together with each recent
    /// deletion: everything deleted since then and still missing, against what was tracked
    /// then. Reports the widest span over the threshold, and returns this capture's deletion
    /// to [`DeleteLog::commit`] once the capture is stored.
    pub fn check(&self, now: i64, prev: &WorkMap, next: &WorkMap) -> (GuardVerdict, Option<Deletion>) {
        let paths: Vec<RelPath> = prev.keys().filter(|p| !next.contains_key(*p)).cloned().collect();
        let mut verdict = judge(paths.len(), prev.len());
        let mut lost = paths.len();
        for d in self.0.iter().rev().filter(|d| now - d.time < MASS_DELETE_WINDOW_SECS) {
            lost += d.paths.iter().filter(|p| !next.contains_key(*p) && paths.binary_search(p).is_err()).count();
            if let v @ GuardVerdict::MassDelete { .. } = judge(lost, d.total) {
                verdict = v;
            }
        }
        let d = (!paths.is_empty()).then(|| Deletion { time: now, total: next.len() + paths.len(), paths });
        (verdict, d)
    }

    /// Records `d` from a stored capture whose result is `next`; forgets deletions that left the
    /// window and paths that are back.
    pub fn commit(&mut self, now: i64, next: &WorkMap, d: Option<Deletion>) {
        if self.0.is_empty() && d.is_none() {
            return;
        }
        let again = d.as_ref().map_or(&[][..], |d| &d.paths[..]);
        self.0.retain(|e| now - e.time < MASS_DELETE_WINDOW_SECS);
        for e in &mut self.0 {
            e.paths.retain(|p| !next.contains_key(p) && again.binary_search(p).is_err());
        }
        self.0.extend(d);
    }

    /// The user confirmed the deletions (`spoko resume --confirm`): start counting afresh.
    pub fn clear(&mut self) {
        self.0.clear();
    }
}
