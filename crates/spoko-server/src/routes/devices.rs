use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use spoko_proto::{BootstrapRequest, DeviceCredentials, DeviceInfo, KeyringUpdate, RecoveryBlob, WrappedKeyring};
use uuid::Uuid;

use crate::Shared;
use crate::auth::{AuthDevice, BootstrapAuth};
use crate::error::{ApiError, ApiResult};

pub async fn bootstrap(State(s): State<Shared>, auth: BootstrapAuth, Json(req): Json<BootstrapRequest>) -> ApiResult<Json<DeviceCredentials>> {
    let account = s.db.consume_bootstrap_token(&auth.token)?.ok_or(ApiError::Unauthorized)?;
    if let Some(r) = &req.recovery_wrapped_keyring {
        s.db.set_recovery(account, r)?;
    }
    Ok(Json(s.db.create_device(account, &req.device_name, &req.device_pubkey, &req.wrapped_keyring)?))
}

pub async fn recovery(State(s): State<Shared>, auth: BootstrapAuth) -> ApiResult<Json<RecoveryBlob>> {
    let blob = s.db.get_recovery(auth.account_id)?.ok_or(ApiError::NotFound)?;
    Ok(Json(RecoveryBlob { recovery_wrapped_keyring: blob }))
}

pub async fn list(State(s): State<Shared>, auth: AuthDevice) -> ApiResult<Json<Vec<DeviceInfo>>> {
    Ok(Json(s.db.list_devices(auth.account_id)?))
}

pub async fn my_keyring(State(s): State<Shared>, auth: AuthDevice) -> ApiResult<Json<WrappedKeyring>> {
    let wrapped_keyring = s.db.device_keyring(auth.device_id)?.ok_or(ApiError::NotFound)?;
    Ok(Json(WrappedKeyring { wrapped_keyring }))
}

pub async fn revoke(State(s): State<Shared>, auth: AuthDevice, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    if s.db.revoke_device(auth.account_id, id)? { Ok(StatusCode::NO_CONTENT) } else { Err(ApiError::NotFound) }
}

pub async fn update_keyrings(State(s): State<Shared>, auth: AuthDevice, Json(req): Json<KeyringUpdate>) -> ApiResult<StatusCode> {
    let per_device: Vec<(Uuid, Vec<u8>)> = req.per_device.into_iter().map(|d| (d.device_id, d.wrapped_keyring)).collect();
    s.db.update_keyrings(auth.account_id, &per_device, req.recovery_wrapped_keyring.as_deref())?;
    Ok(StatusCode::NO_CONTENT)
}
