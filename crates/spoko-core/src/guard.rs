//! Refuse to propagate a sudden mass deletion (unmounted disk, `rm -rf` by accident).

use crate::store::WorkMap;

pub const MASS_DELETE_RATIO: f64 = 0.30;
pub const MASS_DELETE_MIN: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardVerdict {
    Ok,
    MassDelete { deleted: usize, total: usize },
}

pub fn check_deletions(prev: &WorkMap, next: &WorkMap) -> GuardVerdict {
    let total = prev.len();
    let deleted = prev.keys().filter(|p| !next.contains_key(*p)).count();
    #[allow(clippy::cast_precision_loss)]
    let ratio = if total == 0 { 0.0 } else { deleted as f64 / total as f64 };
    if deleted > MASS_DELETE_MIN && ratio > MASS_DELETE_RATIO { GuardVerdict::MassDelete { deleted, total } } else { GuardVerdict::Ok }
}
