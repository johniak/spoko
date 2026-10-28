//! spoko server: encrypted blob store, CAS pointers and change notifications.

pub mod auth;
pub mod blobstore;
pub mod cli;
pub mod db;
pub mod error;
pub mod hub;
mod routes;
#[cfg(feature = "test-util")]
pub mod test_util;

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;

pub use routes::router;

pub struct AppState {
    pub db: db::Db,
    pub blobs: blobstore::BlobStore,
    pub hub: hub::Hub,
}

pub type Shared = Arc<AppState>;

pub fn app(state: Shared) -> Router {
    Router::new()
        .nest(spoko_proto::API_PREFIX, router())
        .layer(DefaultBodyLimit::max(spoko_proto::MAX_BATCH_BYTES + (1 << 20)))
        .with_state(state)
}

pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after 1970")
        .as_secs()
        .cast_signed()
}
