//! HTTP-facing error type.
//!
//! Handlers return `ApiError`; `IntoResponse` serialises it as
//! `{code, message, hint}` with the matching status. Messages are safe to show
//! to users; internal details belong in logs, not in this body.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

/// JSON error envelope returned to clients.
#[derive(Debug, Clone, Serialize)]
pub struct ApiErrorBody {
    pub code: &'static str,
    pub message: String,
    pub hint: Option<String>,
}

/// Error with an HTTP status and a stable machine-readable code.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub hint: Option<String>,
}

impl ApiError {
    /// 400 for malformed input.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "bad_request",
            message: message.into(),
            hint: None,
        }
    }

    /// 413 for oversized queries.
    pub fn payload_too_large(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "too_large",
            message: message.into(),
            hint: None,
        }
    }

    /// 429 when concurrent-run capacity is exhausted.
    pub fn rate_limited(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "rate_limited",
            message: message.into(),
            hint: None,
        }
    }

    /// 429 when the daily token or USD budget is spent.
    pub fn budget_exhausted(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "budget_exhausted",
            message: message.into(),
            hint: Some("Попробуйте завтра или обратитесь к куратору за лимитом".to_string()),
        }
    }

    /// 401 when `APP_TOKEN` is required but missing or wrong.
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthorized",
            message: message.into(),
            hint: Some(
                "Передайте APP_TOKEN в заголовке x-app-token или Authorization: Bearer".to_string(),
            ),
        }
    }

    /// 500 for unexpected server failures.
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "internal",
            message: message.into(),
            hint: None,
        }
    }
}

/// Serialises the error as JSON with its status code.
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ApiErrorBody {
            code: self.code,
            message: self.message,
            hint: self.hint,
        };
        (self.status, Json(body)).into_response()
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}
