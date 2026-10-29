use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use uuid::Uuid;

use crate::error::ApiError;
use crate::{Shared, now};

/// An authenticated, non-revoked device.
#[derive(Debug, Clone, Copy)]
pub struct AuthDevice {
    pub device_id: Uuid,
    pub account_id: Uuid,
}

pub fn bearer(parts: &Parts) -> Option<&str> {
    parts.headers.get("authorization")?.to_str().ok()?.strip_prefix("Bearer ")
}

impl FromRequestParts<Shared> for AuthDevice {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &Shared) -> Result<Self, Self::Rejection> {
        let token = bearer(parts).ok_or(ApiError::Unauthorized)?;
        let row = state.db.device_by_token(token)?.ok_or(ApiError::Unauthorized)?;
        state.db.touch_device(row.id, now())?;
        Ok(AuthDevice { device_id: row.id, account_id: row.account_id })
    }
}

/// This server as the client names it (what login and rotation signatures are bound to): the
/// configured public URL, else the request's `Host` (h2: the URI authority).
pub struct ServerHost(pub String);

impl FromRequestParts<Shared> for ServerHost {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &Shared) -> Result<Self, Self::Rejection> {
        let header = || parts.headers.get(axum::http::header::HOST).and_then(|h| h.to_str().ok()).map(str::to_owned);
        let host = state.public_url.clone().or_else(header).or_else(|| parts.uri.authority().map(ToString::to_string));
        host.map(ServerHost).ok_or(ApiError::BadRequest)
    }
}

impl AuthDevice {
    /// Ensures the project belongs to this device's account (404 otherwise, to not leak existence).
    pub fn check_project(&self, state: &Shared, project: Uuid) -> Result<(), ApiError> {
        match state.db.project_account(project)? {
            Some(acc) if acc == self.account_id => Ok(()),
            _ => Err(ApiError::NotFound),
        }
    }
}
