use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use spoko_proto::{AccountKeyring, Challenge, ChallengeRequest, DeviceInfo, LoginRequest, LoginResponse, RotateKey, SignPurpose, signed_message};
use uuid::Uuid;

use crate::auth::{AuthDevice, ServerHost};
use crate::db::Rotation;
use crate::error::{ApiError, ApiResult};
use crate::{Shared, now};

/// Issued for any well-formed key, so the answer does not tell whether an account has it.
pub async fn challenge(State(s): State<Shared>, Json(req): Json<ChallengeRequest>) -> ApiResult<Json<Challenge>> {
    let (key, _) = spoko_crypto::parse_public_key(&req.public_key).map_err(|_| ApiError::BadRequest)?;
    Ok(Json(s.db.create_challenge(&key, now())?))
}

fn verify(key: &str, purpose: SignPurpose, host: &ServerHost, nonce: &[u8], signature: &str) -> ApiResult<()> {
    spoko_crypto::verify_signature(key, &signed_message(purpose, &host.0, nonce), signature).map_err(|_| ApiError::Unauthorized)
}

pub async fn login(State(s): State<Shared>, host: ServerHost, Json(req): Json<LoginRequest>) -> ApiResult<Json<LoginResponse>> {
    let (key, nonce) = s.db.take_challenge(req.challenge_id, now())?.ok_or(ApiError::Unauthorized)?;
    verify(&key, SignPurpose::Login, &host, &nonce, &req.signature)?;
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

/// 401 unless the account's current key and the new key each signed their challenge (a
/// session token alone must not move the account); 409 when another account owns the new key.
pub async fn rotate_key(State(s): State<Shared>, auth: AuthDevice, host: ServerHost, Json(req): Json<RotateKey>) -> ApiResult<StatusCode> {
    // both are used up, even when the other one or a signature fails
    let old = s.db.take_challenge(req.old_challenge_id, now())?;
    let new = s.db.take_challenge(req.challenge_id, now())?;
    let ((old_key, old_nonce), (key, nonce)) = old.zip(new).ok_or(ApiError::Unauthorized)?;
    verify(&old_key, SignPurpose::RotateOld, &host, &old_nonce, &req.old_signature)?;
    verify(&key, SignPurpose::RotateNew, &host, &nonce, &req.signature)?;
    if key == old_key {
        return Err(ApiError::BadRequest);
    }
    let (_, fingerprint) = spoko_crypto::parse_public_key(&key).map_err(|_| ApiError::BadRequest)?;
    match s.db.rotate_key(auth.account_id, auth.device_id, &old_key, &key, &fingerprint, &req.ssh_wrapped, &req.recovery_wrapped)? {
        Rotation::Done => Ok(StatusCode::NO_CONTENT),
        Rotation::KeyTaken => Err(ApiError::Exists),
        // the old key is not (or no longer) the account's
        Rotation::Stale => Err(ApiError::Unauthorized),
    }
}

pub async fn list(State(s): State<Shared>, auth: AuthDevice) -> ApiResult<Json<Vec<DeviceInfo>>> {
    Ok(Json(s.db.list_devices(auth.account_id)?))
}

pub async fn revoke(State(s): State<Shared>, auth: AuthDevice, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    if s.db.revoke_device(auth.account_id, id)? { Ok(StatusCode::NO_CONTENT) } else { Err(ApiError::NotFound) }
}
