//! The single error format (AGENT.md §1 rule 7):
//! `{"error": {"code": "<machine code>", "message": "<human text>"}}` with a matching HTTP status.
//! Internal details are logged server-side (without secrets) and never returned.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use nc_protocol::pb::v1::{ErrorEnvelope, ErrorResponse};

/// API error.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
    pub fn bad_request(m: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", m)
    }
    pub fn unauthorized(m: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", m)
    }
    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", "blob access denied")
    }
    pub fn range() -> Self {
        Self::new(
            StatusCode::RANGE_NOT_SATISFIABLE,
            "invalid_range",
            "invalid byte range",
        )
    }
    pub fn not_found(m: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", m)
    }
    pub fn conflict(code: &'static str, m: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, m)
    }
    pub fn too_large() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "request body too large",
        )
    }
    pub fn method_not_allowed() -> Self {
        Self::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "method not allowed",
        )
    }
    pub fn unsupported_media() -> Self {
        Self::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "use application/json",
        )
    }
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "internal error",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorResponse {
            error: Some(ErrorEnvelope {
                code: self.code.into(),
                message: self.message,
            }),
        };
        (self.status, Json(body)).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(_e: sqlx::Error) -> Self {
        tracing::error!("database operation failed");
        ApiError::internal()
    }
}

impl From<std::io::Error> for ApiError {
    fn from(_: std::io::Error) -> Self {
        Self::internal()
    }
}
