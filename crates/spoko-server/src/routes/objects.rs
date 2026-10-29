use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use spoko_proto::{MAX_OBJECT_SIZE, MissingRequest, MissingResponse, decode_batch, encode_batch};
use uuid::Uuid;

use crate::Shared;
use crate::auth::AuthDevice;
use crate::error::{ApiError, ApiResult};

pub async fn missing(State(s): State<Shared>, auth: AuthDevice, Path(p): Path<Uuid>, Json(req): Json<MissingRequest>) -> ApiResult<Json<MissingResponse>> {
    auth.check_project(&s, p)?;
    let s2 = s.clone();
    let ids = tokio::task::spawn_blocking(move || s2.blobs.missing(p, &req.ids)).await??;
    Ok(Json(MissingResponse { ids }))
}

pub async fn upload(State(s): State<Shared>, auth: AuthDevice, Path(p): Path<Uuid>, body: Bytes) -> ApiResult<StatusCode> {
    auth.check_project(&s, p)?;
    let items = decode_batch(&body).map_err(|_| ApiError::BadRequest)?;
    if items.iter().any(|(_, b)| b.len() > MAX_OBJECT_SIZE) {
        return Err(ApiError::TooLarge);
    }
    let s2 = s.clone();
    tokio::task::spawn_blocking(move || s2.blobs.put_batch(p, &items)).await??;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn fetch(State(s): State<Shared>, auth: AuthDevice, Path(p): Path<Uuid>, Json(req): Json<MissingRequest>) -> ApiResult<Vec<u8>> {
    auth.check_project(&s, p)?;
    let s2 = s.clone();
    let found = tokio::task::spawn_blocking(move || s2.blobs.get_many(p, &req.ids)).await??;
    Ok(encode_batch(&found))
}
