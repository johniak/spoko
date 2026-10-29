//! Core of the spoko client: config, filtering, snapshots, merge, git state.

pub mod checkout;
pub mod config;
pub mod filter;
pub mod gitstate;
pub mod merge;
pub mod platform;
pub mod store;
pub mod serde_oid;
pub mod scan;

/// Error type shared by the core modules.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("git: {0}")]
    Git(#[from] git2::Error),
    #[error("config: {0}")]
    Config(#[from] config::ConfigError),
    #[error("ignore pattern: {0}")]
    Ignore(#[from] ignore::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
