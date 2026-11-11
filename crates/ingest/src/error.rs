//! How a refused request is answered. gsd reads the status (see the table in
//! `docs/backend.md`); the body is for people.

use std::time::Duration;

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use groundstation_wire::ErrorBody;

use crate::auth::AuthError;
use crate::body::BodyError;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    message: String,
    retry_after: Option<Duration>,
}

impl ApiError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            retry_after: None,
        }
    }

    pub fn rate_limited(wait: Duration) -> Self {
        Self {
            retry_after: Some(wait),
            ..Self::new(StatusCode::TOO_MANY_REQUESTS, "rate limited")
        }
    }

    /// Too many batches at once on this instance: come back shortly.
    pub fn busy() -> Self {
        Self {
            retry_after: Some(Duration::from_secs(1)),
            ..Self::new(StatusCode::TOO_MANY_REQUESTS, "busy, retry shortly")
        }
    }

    /// Storage is down: nothing is wrong with the batch, so gsd retries it.
    pub fn unavailable(error: impl std::fmt::Display) -> Self {
        tracing::error!("storage unavailable: {error}");
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "storage unavailable, retry later",
        )
    }
}

impl From<AuthError> for ApiError {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::Missing => Self::new(StatusCode::UNAUTHORIZED, "missing bearer token"),
            AuthError::Unknown => {
                Self::new(StatusCode::UNAUTHORIZED, "unknown or revoked ingest token")
            }
            AuthError::Suspended => Self::new(StatusCode::FORBIDDEN, "tenant suspended"),
            AuthError::Busy(wait) => Self::rate_limited(wait),
            AuthError::Unavailable(e) => Self::unavailable(e),
        }
    }
}

impl From<BodyError> for ApiError {
    fn from(e: BodyError) -> Self {
        match e {
            BodyError::TooLarge => Self::new(StatusCode::PAYLOAD_TOO_LARGE, "batch too large"),
            BodyError::UnsupportedEncoding => Self::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "content-encoding must be gzip or identity",
            ),
            BodyError::Corrupt => Self::new(StatusCode::BAD_REQUEST, "body is not valid gzip"),
            BodyError::NotABatch => Self::new(StatusCode::BAD_REQUEST, "body is not a batch"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response();
        if let Some(wait) = self.retry_after {
            let secs = wait.as_secs_f64().ceil().max(1.0) as u64;
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(secs));
        }
        response
    }
}
