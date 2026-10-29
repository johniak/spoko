use std::io::Read;

use serde::{Deserialize, Serialize};

use crate::api::ImageInfo;

/// What was known about a volume snapshot when it was taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeMeta {
    pub image_tags: Vec<String>,
    pub image_id: String,
    pub arch: String,
    pub os: String,
    /// Postgres major version (`PG_VERSION` in the data directory), when present.
    pub pg_major: Option<String>,
    pub created_at: i64,
    pub device_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mismatch {
    Arch { local: String, snapshot: String },
    /// Different images (e.g. debian vs alpine): collations may differ.
    ImageTag { local: Vec<String>, snapshot: Vec<String> },
    PgMajor { local: String, snapshot: String },
    /// Config wants `dump` but only a raw snapshot exists (or the other way round).
    Mode,
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mismatch::Arch { local, snapshot } => write!(f, "architektura: lokalnie {local}, w snapshocie {snapshot}"),
            Mismatch::ImageTag { local, snapshot } => write!(f, "obraz: lokalnie {local:?}, w snapshocie {snapshot:?}"),
            Mismatch::PgMajor { local, snapshot } => write!(f, "Postgres: lokalnie {local}, w snapshocie {snapshot}"),
            Mismatch::Mode => f.write_str("snapshot jest w innym trybie (raw/dump) niż konfiguracja"),
        }
    }
}

/// Major version from an image tag like `postgres:16.4-alpine` → `16`.
fn tag_major(tags: &[String]) -> Option<String> {
    tags.iter().find_map(|t| {
        let tag = t.rsplit_once(':')?.1;
        let digits: String = tag.chars().take_while(char::is_ascii_digit).collect();
        (!digits.is_empty()).then_some(digits)
    })
}

pub fn compare(local: &ImageInfo, snap: &VolumeMeta) -> Vec<Mismatch> {
    let mut out = Vec::new();
    if local.arch != snap.arch {
        out.push(Mismatch::Arch { local: local.arch.clone(), snapshot: snap.arch.clone() });
    }
    if !local.tags.iter().any(|t| snap.image_tags.contains(t)) {
        out.push(Mismatch::ImageTag { local: local.tags.clone(), snapshot: snap.image_tags.clone() });
    }
    if let (Some(l), Some(s)) = (tag_major(&local.tags), &snap.pg_major)
        && &l != s
    {
        out.push(Mismatch::PgMajor { local: l, snapshot: s.clone() });
    }
    out
}

/// `PG_VERSION` content from a volume tar (searched up to 4 path components deep, which covers
/// `spoko-vol/PG_VERSION` and postgres 18's `spoko-vol/18/docker/PG_VERSION`). Not a tar → None.
pub fn pg_major_from_tar(tar: impl Read) -> Option<String> {
    let mut archive = tar::Archive::new(tar);
    for entry in archive.entries().ok()? {
        let mut e = entry.ok()?;
        let path = e.path().ok()?.into_owned();
        if path.file_name().is_some_and(|n| n == "PG_VERSION") && path.components().count() <= 4 {
            let mut s = String::new();
            e.read_to_string(&mut s).ok()?;
            return Some(s.trim().to_owned());
        }
    }
    None
}
