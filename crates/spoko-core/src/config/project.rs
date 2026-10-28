use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Deserializer};

use super::ConfigError;

pub const CONFIG_FILE: &str = ".spokoconfig";

/// `.spokoconfig` in the project root. Every field has a default.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectConfig {
    pub ignore: IgnoreCfg,
    pub binary: BinaryCfg,
    pub watch: Vec<WatchCfg>,
    pub docker: DockerCfg,
    pub external: ExternalCfg,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IgnoreCfg {
    pub inherit_gitignore: bool,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl Default for IgnoreCfg {
    fn default() -> Self {
        IgnoreCfg { inherit_gitignore: true, include: vec![], exclude: vec![] }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BinaryCfg {
    pub patterns: Vec<String>,
    pub size_threshold: ByteSize,
}

impl Default for BinaryCfg {
    fn default() -> Self {
        BinaryCfg { patterns: vec![], size_threshold: ByteSize(10_000_000) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HookMode {
    #[default]
    Ask,
    Auto,
    Notify,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WatchCfg {
    pub files: Vec<String>,
    #[serde(default)]
    pub run: Option<String>,
    #[serde(default)]
    pub run_macos: Option<String>,
    #[serde(default)]
    pub run_linux: Option<String>,
    #[serde(default)]
    pub run_windows: Option<String>,
    #[serde(default)]
    pub mode: HookMode,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DockerCfg {
    pub volume: Vec<VolumeCfg>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VolumeMode {
    #[default]
    Raw,
    Dump,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mismatch {
    #[default]
    Warn,
    Dump,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeCfg {
    pub name: String,
    #[serde(default)]
    pub mode: VolumeMode,
    #[serde(default = "yes")]
    pub stop_container: bool,
    #[serde(default)]
    pub dump: Option<String>,
    #[serde(default)]
    pub restore: Option<String>,
    #[serde(default)]
    pub on_mismatch: Mismatch,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExternalCfg {
    pub claude_code: Toggle,
    pub custom: Vec<CustomExternal>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Toggle {
    pub enabled: bool,
}

impl Default for Toggle {
    fn default() -> Self {
        Toggle { enabled: true }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomExternal {
    pub name: String,
    /// May contain `{home}`.
    pub path: String,
    #[serde(default)]
    pub rewrite_paths: bool,
}

impl ProjectConfig {
    pub fn load(project_root: &Path) -> Result<ProjectConfig, ConfigError> {
        match std::fs::read_to_string(project_root.join(CONFIG_FILE)) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ProjectConfig::default()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Size in bytes; parses `"10MB"`, `"512KiB"`, `"1 GiB"` or a plain integer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ByteSize(pub u64);

impl FromStr for ByteSize {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
        let (num, unit) = s.split_at(split);
        let n: u64 = num.parse().map_err(|_| format!("invalid size: {s}"))?;
        let mult = match unit.trim().to_ascii_lowercase().as_str() {
            "" | "b" => 1,
            "kb" => 1000,
            "mb" => 1000 * 1000,
            "gb" => 1000 * 1000 * 1000,
            "kib" => 1 << 10,
            "mib" => 1 << 20,
            "gib" => 1 << 30,
            other => return Err(format!("unknown size unit: {other}")),
        };
        Ok(ByteSize(n * mult))
    }
}

impl<'de> Deserialize<'de> for ByteSize {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Int(u64),
            Str(String),
        }
        match Raw::deserialize(d)? {
            Raw::Int(n) => Ok(ByteSize(n)),
            Raw::Str(s) => s.parse().map_err(serde::de::Error::custom),
        }
    }
}
