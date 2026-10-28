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
    let ids = tokio::task::spawn_blocking(move || req.ids.into_iter().filter(|id| !s2.blobs.exists(p, id)).collect()).await?;
    Ok(Json(MissingResponse { ids }))
}

pub async fn upload(State(s): State<Shared>, auth: AuthDevice, Path(p): Path<Uuid>, body: Bytes) -> ApiResult<StatusCode> {
    auth.check_project(&s, p)?;
    let items = decode_batch(&body).map_err(|_| ApiError::BadRequest)?;
    if items.iter().any(|(_, b)| b.len() > MAX_OBJECT_SIZE) {
        return Err(ApiError::TooLarge);
    }
    let s2 = s.clone();
    tokio::task::spawn_blocking(move || items.iter().try_for_each(|(id, b)| s2.blobs.put(p, id, b))).await??;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn fetch(State(s): State<Shared>, auth: AuthDevice, Path(p): Path<Uuid>, Json(req): Json<MissingRequest>) -> ApiResult<Vec<u8>> {
    auth.check_project(&s, p)?;
    let s2 = s.clone();
    let found = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<_>> {
        let mut out = Vec::new();
        for id in req.ids {
            if let Some(b) = s2.blobs.get(p, &id)? {
                out.push((id, b));
            }
        }
        Ok(out)
    })
    .await??;
    Ok(encode_batch(&found))
}
