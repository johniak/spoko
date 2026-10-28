//! Cross-platform path handling and safe file operations.

mod casefold;
mod fsops;
mod names;

pub use casefold::{find_collisions, fold_key, fs_is_case_insensitive};
pub use fsops::*;
pub use names::{NameProblem, RelPath, windows_problem};
