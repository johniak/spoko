//! Docker volume sync: snapshots of volumes (raw tar or dump/restore) through the encrypted
//! chunk store, so a database follows the developer between machines.

pub mod api;
pub mod bollard_api;
pub mod fake;
pub mod meta;
pub mod volume;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("docker: {0}")]
    Docker(String),
    #[error(transparent)]
    Client(#[from] spoko_client::ClientError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("volume {0} not found")]
    NotFound(String),
    #[error("volume name {logical} is ambiguous: {candidates:?}")]
    Ambiguous { logical: String, candidates: Vec<String> },
    #[error("{0}")]
    Other(String),
}

impl From<spoko_crypto::CryptoError> for Error {
    fn from(e: spoko_crypto::CryptoError) -> Self {
        Error::Client(e.into())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
