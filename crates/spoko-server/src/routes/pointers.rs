use axum::Json;
use axum::extract::{Path, State};
use spoko_proto::{Pointer, PointerPut, ServerEvent};
use uuid::Uuid;

use crate::Shared;
use crate::auth::AuthDevice;
use crate::error::{ApiError, ApiResult};

pub async fn get(State(s): State<Shared>, auth: AuthDevice, Path((p, name)): Path<(Uuid, String)>) -> ApiResult<Json<Pointer>> {
    auth.check_project(&s, p)?;
    s.db.get_pointer(p, &name)?.map(Json).ok_or(ApiError::NotFound)
}

pub async fn put(State(s): State<Shared>, auth: AuthDevice, Path((p, name)): Path<(Uuid, String)>, Json(req): Json<PointerPut>) -> ApiResult<Json<Pointer>> {
    auth.check_project(&s, p)?;
    let next = s.db.cas_pointer(p, &name, req.expected_version, &req.value)?.map_err(ApiError::Conflict)?;
    s.hub.publish(auth.account_id, Some(auth.device_id), ServerEvent::PointerChanged { project: p, name, version: next.version });
    Ok(Json(next))
}
