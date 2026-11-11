//! `POST /v1/events`: a batch from gsd (or an SDK) into ClickHouse.
//!
//! Problems with the whole request get a non-2xx status and change nothing.
//! Problems with single events come back in `rejected` inside a 200, with the
//! rest of the batch stored. Storage trouble is always a 503, never a
//! rejection: gsd drops events that are rejected repeatedly.
//!
//! Inflating and parsing a batch is CPU work on up to 64 MiB, so it runs off
//! the async threads, and only so many batches at once.

use std::time::Instant;

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use chrono::Utc;
use groundstation_backend::ch::{EventRow, Stamp};
use groundstation_backend::pg::tokens::Grant;
use groundstation_schema::SCHEMA;
use groundstation_wire::{IngestResponse, Rejection};

use crate::AppState;
use crate::body::{self, BodyError, Envelope};
use crate::error::ApiError;
use crate::validate::{self, Invalid};

pub async fn ingest(
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<IngestResponse>, ApiError> {
    let started = Instant::now();
    let mut seen = Seen::default();
    let result = handle(&s, &headers, body, &mut seen).await;
    seen.log(&result, started);
    result.map(Json)
}

async fn handle(
    s: &AppState,
    headers: &HeaderMap,
    body: Body,
    seen: &mut Seen,
) -> Result<IngestResponse, ApiError> {
    let grant = s.auth.authenticate(headers).await?;
    seen.grant = Some(grant.clone());
    // Bodies held at once are bounded, and so is how long one takes to arrive.
    // Before the rate limits: a request refused because this instance is busy
    // mustn't cost the client its own quota.
    let _held = s
        .held
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::busy())?;
    s.limits.check(&grant, Instant::now())?;
    let bytes = tokio::time::timeout(s.read_timeout, read(body))
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::REQUEST_TIMEOUT,
                "the batch took too long to arrive",
            )
        })??;
    seen.bytes = bytes.len();
    let encoding = headers
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    // The permit goes with the work, and then with the rows until they are
    // stored: a client that hangs up can't free a slot while its batch is
    // still being inflated or inserted.
    let permit = s.decoding.clone().acquire_owned().await.map_err(internal)?;
    let decoded = tokio::task::spawn_blocking(move || {
        decode(encoding.as_deref(), &bytes, &grant).map(|decoded| (decoded, permit))
    })
    .await
    .map_err(internal)?;
    let ((rows, rejected), permit) = decoded?;
    seen.events = rows.len() + rejected.len();
    s.clickhouse
        .insert_events(&rows)
        .await
        .map_err(ApiError::unavailable)?;
    drop(permit);
    Ok(IngestResponse {
        stored: rows.len(),
        rejected,
    })
}

/// A bug here, not a bad batch: gsd retries.
fn internal(error: impl std::fmt::Display) -> ApiError {
    tracing::error!("decoding a batch: {error}");
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
}

/// The body as sent, to rows and refusals.
fn decode(
    encoding: Option<&str>,
    bytes: &[u8],
    grant: &Grant,
) -> Result<(Vec<EventRow>, Vec<Rejection>), ApiError> {
    let json = body::decode(encoding, bytes)?;
    let batch = body::envelope(&json)?;
    if batch.schema != SCHEMA {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("unknown schema; this backend accepts {SCHEMA}"),
        ));
    }
    prepare(&batch, grant)
}

async fn read(body: Body) -> Result<axum::body::Bytes, ApiError> {
    axum::body::to_bytes(body, body::MAX_COMPRESSED)
        .await
        .map_err(|e| {
            let too_large = std::error::Error::source(&e)
                .is_some_and(|e| e.is::<http_body_util::LengthLimitError>());
            match too_large {
                true => BodyError::TooLarge.into(),
                false => ApiError::new(StatusCode::BAD_REQUEST, "could not read the body"),
            }
        })
}

fn prepare(
    batch: &Envelope<'_>,
    grant: &Grant,
) -> Result<(Vec<EventRow>, Vec<Rejection>), ApiError> {
    let now = Utc::now();
    let stamp = Stamp::new(grant.tenant_id, grant.token_id, grant.retention_days, now);
    let mut rows = Vec::with_capacity(batch.events.len());
    let mut rejected = Vec::new();
    for (i, raw) in batch.events.iter().enumerate() {
        match validate::to_row(raw, &batch.schema, stamp, now) {
            Ok(row) => rows.push(row),
            Err(Invalid::Event { id, error }) => rejected.push(Rejection {
                id,
                error: error.to_string(),
            }),
            // A client bug, not a bad event: nothing to refuse it by.
            Err(Invalid::NoId) => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!("event {i} has no valid id"),
                ));
            }
        }
    }
    Ok((rows, rejected))
}

/// What one request's log line says: who, how much, what happened. Never
/// content.
#[derive(Default)]
struct Seen {
    grant: Option<Grant>,
    bytes: usize,
    events: usize,
}

impl Seen {
    fn log(&self, result: &Result<IngestResponse, ApiError>, started: Instant) {
        let ms = started.elapsed().as_millis() as u64;
        let (status, stored, rejected) = match result {
            Ok(r) => (200, r.stored, r.rejected.len()),
            Err(e) => (e.status.as_u16(), 0, 0),
        };
        let Some(grant) = &self.grant else {
            // Refused before anyone could be named: an unbounded stream of
            // these must not fill the disk.
            tracing::debug!(ms, "{status} POST /v1/events");
            return;
        };
        tracing::info!(
            tenant = %grant.tenant_id,
            token = %grant.token_id,
            bytes = self.bytes,
            events = self.events,
            stored,
            rejected,
            ms,
            "{status} POST /v1/events"
        );
    }
}
