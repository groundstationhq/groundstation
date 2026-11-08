//! Errors as `/v1` answers them: a status and `{"error": "..."}`, the same
//! shape gsd uses.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use groundstation_wire::ErrorBody;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    pub fn not_found(what: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, format!("{what} not found"))
    }
}

/// A database error is never the caller's fault, and its text stays in the log.
impl From<groundstation_backend::Error> for ApiError {
    fn from(e: groundstation_backend::Error) -> Self {
        tracing::error!("storage: {e}");
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "storage unavailable, retry later",
        )
    }
}

/// A body that isn't the JSON the endpoint takes. Fixed words: axum's own
/// message would quote what was sent.
impl From<axum::extract::rejection::JsonRejection> for ApiError {
    fn from(rejection: axum::extract::rejection::JsonRejection) -> Self {
        let status = rejection.status();
        let message = match status {
            StatusCode::UNSUPPORTED_MEDIA_TYPE => "send JSON, with content-type: application/json",
            _ => "the body isn't the JSON this endpoint takes",
        };
        Self::new(status, message)
    }
}

/// A query string the endpoint can't read, in the same fixed words on both
/// hosts.
impl From<axum::extract::rejection::QueryRejection> for ApiError {
    fn from(_: axum::extract::rejection::QueryRejection) -> Self {
        Self::new(StatusCode::BAD_REQUEST, groundstation_wire::BAD_QUERY)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}
