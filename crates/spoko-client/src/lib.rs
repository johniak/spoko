//! spoko client: transfer, sync engine, daemon.

#[cfg(feature = "test-util")]
pub mod test_util;
pub mod transfer;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(transparent)]
    Core(#[from] spoko_core::Error),
    #[error("git: {0}")]
    Git(#[from] git2::Error),
    #[error("crypto: {0}")]
    Crypto(#[from] spoko_crypto::CryptoError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("server: {0}")]
    Server(String),
    #[error("integrity: {0}")]
    Integrity(String),
}

pub type Result<T> = std::result::Result<T, ClientError>;
