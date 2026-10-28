//! Wire types shared by the spoko client and server.

mod api;
mod events;
mod framing;
pub mod serde_bytes;

pub use api::*;
pub use events::ServerEvent;
pub use framing::{FramingError, decode_batch, encode_batch};

pub const MAX_OBJECT_SIZE: usize = 8 * 1024 * 1024;
pub const MAX_BATCH_BYTES: usize = 32 * 1024 * 1024;
pub const API_PREFIX: &str = "/v1";
