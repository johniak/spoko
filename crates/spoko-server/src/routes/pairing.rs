use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use spoko_proto::{PairingApprove, PairingInfo, PairingStart, PairingStarted, PairingStatus};
use uuid::Uuid;

use crate::auth::AuthDevice;
use crate::db::PollOutcome;
use crate::error::{ApiError, ApiResult};
use crate::{Shared, now};

pub async fn start(State(s): State<Shared>, Json(req): Json<PairingStart>) -> ApiResult<Json<PairingStarted>> {
    Ok(Json(s.db.create_pairing(&req.device_name, &req.device_pubkey, now())?))
}

#[derive(Deserialize)]
pub struct Secret {
    secret: String,
}

pub async fn poll(State(s): State<Shared>, Path(id): Path<Uuid>, Query(q): Query<Secret>) -> ApiResult<Json<PairingStatus>> {
    match s.db.poll_pairing(id, &q.secret, now())? {
        PollOutcome::Status(st) => Ok(Json(st)),
        PollOutcome::Gone => Err(ApiError::Gone),
        PollOutcome::NotFound => Err(ApiError::NotFound),
    }
}

pub async fn by_code(State(s): State<Shared>, _auth: AuthDevice, Path(code): Path<String>) -> ApiResult<Json<PairingInfo>> {
    s.db.pairing_by_code(&code, now())?.map(Json).ok_or(ApiError::NotFound)
}

pub async fn approve(State(s): State<Shared>, auth: AuthDevice, Path(id): Path<Uuid>, Json(req): Json<PairingApprove>) -> ApiResult<StatusCode> {
    s.db.approve_pairing(id, auth.account_id, &req.wrapped_keyring, now())?.ok_or(ApiError::NotFound)?;
    Ok(StatusCode::NO_CONTENT)
}
