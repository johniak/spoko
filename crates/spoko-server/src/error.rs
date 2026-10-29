use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use spoko_proto::Pointer;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("not found")]
    NotFound,
    #[error("already exists")]
    Exists,
    #[error("bad request")]
    BadRequest,
    #[error("payload too large")]
    TooLarge,
    #[error("conflict")]
    Conflict(Pointer),
    #[error("internal: {0}")]
    Internal(String),
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        ApiError::Internal(e.to_string())
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        ApiError::Internal(e.to_string())
    }
}

impl From<tokio::task::JoinError> for ApiError {
    fn from(e: tokio::task::JoinError) -> Self {
        ApiError::Internal(e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED.into_response(),
            ApiError::NotFound => StatusCode::NOT_FOUND.into_response(),
            ApiError::Exists => StatusCode::CONFLICT.into_response(),
            ApiError::BadRequest => StatusCode::BAD_REQUEST.into_response(),
            ApiError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE.into_response(),
            ApiError::Conflict(p) => (StatusCode::CONFLICT, Json(p)).into_response(),
            ApiError::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
