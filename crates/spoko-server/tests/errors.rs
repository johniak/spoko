use axum::response::IntoResponse;
use spoko_server::error::ApiError;

#[test]
fn internal_errors_map_to_500_with_message() {
    let io: ApiError = std::io::Error::other("disk").into();
    let r = io.into_response();
    assert_eq!(r.status(), 500);
    let sql: ApiError = rusqlite::Error::InvalidQuery.into();
    assert!(matches!(sql, ApiError::Internal(_)));
}

#[tokio::test]
async fn join_errors_map_to_internal() {
    let h = tokio::spawn(async { panic!("boom") });
    let err: ApiError = h.await.unwrap_err().into();
    assert!(matches!(err, ApiError::Internal(_)));
}
