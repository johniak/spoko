use std::collections::BTreeMap;
use std::path::Path;

use super::RelPath;

/// Key under which case-insensitive filesystems consider two paths the same.
pub fn fold_key(rel: &RelPath) -> String {
    rel.as_str().to_lowercase()
}

/// Groups of paths that would collide on a case-insensitive filesystem.
pub fn find_collisions<'a>(paths: impl Iterator<Item = &'a RelPath>) -> Vec<Vec<RelPath>> {
    let mut by_key: BTreeMap<String, Vec<RelPath>> = BTreeMap::new();
    for p in paths {
        by_key.entry(fold_key(p)).or_default().push(p.clone());
    }
    by_key.into_values().filter(|g| g.len() > 1).collect()
}

/// Probes `dir` by creating a mixed-case temp file and looking it up lowercased.
pub fn fs_is_case_insensitive(dir: &Path) -> std::io::Result<bool> {
    let probe = dir.join(format!(".spoko-tmp-CaSe-{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&probe, b"")?;
    let lower = dir.join(probe.file_name().expect("name").to_string_lossy().to_lowercase());
    let result = lower.exists();
    std::fs::remove_file(&probe)?;
    Ok(result)
}
