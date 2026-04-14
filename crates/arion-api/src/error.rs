use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use thiserror::Error;

/// Uniform error envelope for all API responses.
///
/// Shape follows RFC 7807 Problem Details loosely: `{ type, title,
/// status, detail, code }`. `type` is an opaque URL-ish identifier
/// the client can match on; `detail` is human-readable.
#[derive(Debug, Error)]
pub enum ApiError {
    #[error("{0}")]
    Validation(String),

    #[error("{0}")]
    NotFound(String),

    #[error("{0}")]
    Conflict(String),

    #[error("feature disabled: {0}")]
    Disabled(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl ApiError {
    fn parts(&self) -> (StatusCode, &'static str, &'static str) {
        match self {
            ApiError::Validation(_) => (StatusCode::BAD_REQUEST, "validation", "about:blank"),
            ApiError::NotFound(_)   => (StatusCode::NOT_FOUND,   "not_found",  "about:blank"),
            ApiError::Conflict(_)   => (StatusCode::CONFLICT,    "conflict",   "about:blank"),
            ApiError::Disabled(_)   => (StatusCode::FORBIDDEN,   "disabled",   "about:blank"),
            ApiError::Internal(_)   => (StatusCode::INTERNAL_SERVER_ERROR, "internal", "about:blank"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, ty) = self.parts();
        let body = json!({
            "type":   ty,
            "title":  code,
            "status": status.as_u16(),
            "detail": self.to_string(),
            "code":   code,
        });
        (status, Json(body)).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
