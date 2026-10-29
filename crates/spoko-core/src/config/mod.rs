mod local;
mod project;

pub use local::{LocalConfig, LocalProject, home_dir, spoko_home, spoko_home_from};
pub use project::*;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read config: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("cannot write config: {0}")]
    Serialize(#[from] toml::ser::Error),
}
