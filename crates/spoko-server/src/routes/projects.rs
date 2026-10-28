use axum::Json;
use axum::extract::State;
use spoko_proto::{ProjectCreate, ProjectInfo};

use crate::Shared;
use crate::auth::AuthDevice;
use crate::error::ApiResult;

pub async fn create(State(s): State<Shared>, auth: AuthDevice, Json(req): Json<ProjectCreate>) -> ApiResult<Json<ProjectInfo>> {
    let id = s.db.create_project(auth.account_id, &req.encrypted_meta)?;
    Ok(Json(ProjectInfo { id, encrypted_meta: req.encrypted_meta }))
}

pub async fn list(State(s): State<Shared>, auth: AuthDevice) -> ApiResult<Json<Vec<ProjectInfo>>> {
    Ok(Json(s.db.list_projects(auth.account_id)?))
}
