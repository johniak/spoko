use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use spoko_proto::{AccountKeyring, Challenge, ChallengeRequest, DeviceInfo, LoginRequest, LoginResponse, RotateKey};
use uuid::Uuid;

use crate::auth::AuthDevice;
use crate::error::{ApiError, ApiResult};
use crate::{Shared, now};

/// Issued for any well-formed key, so the answer does not tell whether an account has it.
pub async fn challenge(State(s): State<Shared>, Json(req): Json<ChallengeRequest>) -> ApiResult<Json<Challenge>> {
    let (key, _) = spoko_crypto::parse_public_key(&req.public_key).map_err(|_| ApiError::BadRequest)?;
    Ok(Json(s.db.create_challenge(&key, now())?))
}

pub async fn login(State(s): State<Shared>, Json(req): Json<LoginRequest>) -> ApiResult<Json<LoginResponse>> {
    let (key, nonce) = s.db.take_challenge(req.challenge_id, now())?.ok_or(ApiError::Unauthorized)?;
    spoko_crypto::verify_signature(&key, &nonce, &req.signature).map_err(|_| ApiError::Unauthorized)?;
    let account_id = s.db.account_by_key(&key)?.ok_or(ApiError::Unauthorized)?;
    let dev = s.db.login_device(account_id, &req.device_name, req.device_id)?;
    Ok(Json(LoginResponse { account_id, device_id: dev.device_id, token: dev.token, keyring: s.db.account_keyring(account_id)? }))
}

pub async fn keyring(State(s): State<Shared>, auth: AuthDevice) -> ApiResult<Json<AccountKeyring>> {
    s.db.account_keyring(auth.account_id)?.map(Json).ok_or(ApiError::NotFound)
}

/// 409 when the stored keyring is already wrapped for the account's current key.
pub async fn store_keyring(State(s): State<Shared>, auth: AuthDevice, Json(req): Json<AccountKeyring>) -> ApiResult<StatusCode> {
    if s.db.store_keyring(auth.account_id, &req.ssh_wrapped, req.recovery_wrapped.as_deref())? { Ok(StatusCode::NO_CONTENT) } else { Err(ApiError::Exists) }
}

/// 401 unless the new key signed the challenge; 409 when another account owns it.
pub async fn rotate_key(State(s): State<Shared>, auth: AuthDevice, Json(req): Json<RotateKey>) -> ApiResult<StatusCode> {
    let (key, nonce) = s.db.take_challenge(req.challenge_id, now())?.ok_or(ApiError::Unauthorized)?;
    spoko_crypto::verify_signature(&key, &nonce, &req.signature).map_err(|_| ApiError::Unauthorized)?;
    if s.db.account_by_key(&key)? == Some(auth.account_id) {
        return Err(ApiError::BadRequest);
    }
    let (_, fingerprint) = spoko_crypto::parse_public_key(&key).map_err(|_| ApiError::BadRequest)?;
    if s.db.rotate_key(auth.account_id, auth.device_id, &key, &fingerprint, &req.ssh_wrapped, &req.recovery_wrapped)? { Ok(StatusCode::NO_CONTENT) } else { Err(ApiError::Exists) }
}

pub async fn list(State(s): State<Shared>, auth: AuthDevice) -> ApiResult<Json<Vec<DeviceInfo>>> {
    Ok(Json(s.db.list_devices(auth.account_id)?))
}

pub async fn revoke(State(s): State<Shared>, auth: AuthDevice, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    if s.db.revoke_device(auth.account_id, id)? { Ok(StatusCode::NO_CONTENT) } else { Err(ApiError::NotFound) }
}
