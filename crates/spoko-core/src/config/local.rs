use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::ConfigError;

/// Per-machine settings in `$SPOKO_HOME/config.toml` (never synchronized).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalConfig {
    pub server_url: Option<String>,
    pub device_id: Option<Uuid>,
    pub device_name: Option<String>,
    pub projects: BTreeMap<Uuid, LocalProject>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalProject {
    pub path: PathBuf,
    #[serde(default)]
    pub paused: bool,
}

impl LocalConfig {
    pub fn load(home: &Path) -> Result<LocalConfig, ConfigError> {
        match std::fs::read_to_string(home.join("config.toml")) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LocalConfig::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, home: &Path) -> Result<(), ConfigError> {
        std::fs::create_dir_all(home)?;
        let text = toml::to_string_pretty(self)?;
        let tmp = home.join(".spoko-tmp-config.toml");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, home.join("config.toml"))?;
        Ok(())
    }
}

/// `$SPOKO_HOME`, or `~/.spoko`.
pub fn spoko_home() -> PathBuf {
    spoko_home_from(std::env::var_os("SPOKO_HOME"), home_dir())
}

pub fn spoko_home_from(env: Option<OsString>, home: Option<PathBuf>) -> PathBuf {
    match env {
        Some(e) if !e.is_empty() => PathBuf::from(e),
        _ => home.unwrap_or_else(|| PathBuf::from(".")).join(".spoko"),
    }
}

pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}
