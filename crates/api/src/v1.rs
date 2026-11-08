//! The `/v1` read contract, the same paths, parameters and shapes gsd serves,
//! answered for the viewer's tenant.

use axum::body::{Body, Bytes};
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;
use clickhouse::query::RowCursor;
use groundstation_backend::ch::EventRow;
use groundstation_schema::SCHEMA;
use groundstation_wire::{Deployment, Health, ToolStats, TrajectorySummary};
use serde::Deserialize;
use tokio::sync::OwnedSemaphorePermit;

use crate::AppState;
use crate::auth::Viewer;
use crate::error::ApiError;
use crate::queries::stats;
use crate::queries::trajectories::{self, Resolved};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/trajectories", get(list))
        .route("/v1/trajectories/{id}", get(detail))
        .route("/v1/stats/tools", get(tool_stats))
}

/// Optional endpoint groups this host serves, for the UI to feature-detect.
pub const FEATURES: &[&str] = &["insights", "tokens"];

async fn health(State(s): State<AppState>, viewer: Viewer) -> Result<Json<Health>, ApiError> {
    let (trajectories, events) = s.counts(&viewer).await?;
    Ok(Json(Health {
        status: "ok".into(),
        deployment: Deployment::Hosted,
        features: FEATURES.iter().map(|f| f.to_string()).collect(),
        ui: groundstation_ui_embed::EMBEDDED,
        // Agents' hooks go to gsd on their machine, never here.
        adapters: Vec::new(),
        version: env!("CARGO_PKG_VERSION").into(),
        schema: SCHEMA.into(),
        trajectories,
        events,
        pid: None,
        data_dir: None,
        spool_pending: None,
        upload: None,
    }))
}

#[derive(Deserialize)]
struct ListParams {
    /// Any number gsd takes; both clamp it.
    limit: Option<u64>,
}

async fn list(
    State(s): State<AppState>,
    viewer: Viewer,
    params: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<Vec<TrajectorySummary>>, ApiError> {
    let Query(params) = params?;
    let limit = params.limit.unwrap_or(50).clamp(1, 10_000) as u32;
    let (scope, _turn) = s.read(&viewer).await?;
    Ok(Json(trajectories::list(&scope, limit).await?))
}

/// Ingest stores no longer trajectory id, so a longer one matches nothing.
const MAX_ID: usize = 256;

async fn detail(
    State(s): State<AppState>,
    viewer: Viewer,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    if id.len() > MAX_ID {
        return Err(ApiError::not_found("trajectory"));
    }
    let (scope, turn) = s.read(&viewer).await?;
    let id = match trajectories::resolve(&scope, &id).await? {
        Resolved::Found(id) => id,
        Resolved::NotFound => return Err(ApiError::not_found("trajectory")),
        Resolved::Ambiguous(ids) => {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                format!("ambiguous id, matches: {}", ids.join(", ")),
            ));
        }
    };
    let summary = trajectories::summary(&scope, &id)
        .await?
        .ok_or_else(|| ApiError::not_found("trajectory"))?;
    let events = trajectories::events(&scope, &id)?;
    Ok(stream_detail(summary, events, turn))
}

/// A `TrajectoryDetail` as JSON, written while the events are read: the
/// summary's fields, then `events`, one at a time. A timeline can be far
/// larger than anything worth holding in memory. The read's turn is held
/// until the last event is written, or the client has gone.
fn stream_detail(
    summary: TrajectorySummary,
    mut events: RowCursor<EventRow>,
    turn: OwnedSemaphorePermit,
) -> Response {
    let mut head = serde_json::to_vec(&summary).expect("a summary serializes");
    head.pop(); // its closing brace
    head.extend_from_slice(br#","events":["#);
    let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<Bytes>>(16);
    tokio::spawn(async move {
        let _turn = turn;
        if tx.send(Ok(head.into())).await.is_err() {
            return;
        }
        let mut first = true;
        loop {
            let chunk = match events.next().await {
                Ok(Some(row)) => match row.to_event().and_then(|e| serde_json::to_vec(&e)) {
                    Ok(json) => {
                        let mut chunk = Vec::with_capacity(json.len() + 1);
                        if !std::mem::take(&mut first) {
                            chunk.push(b',');
                        }
                        chunk.extend_from_slice(&json);
                        Ok(Bytes::from(chunk))
                    }
                    Err(e) => Err(std::io::Error::other(e)),
                },
                Ok(None) => {
                    let _ = tx.send(Ok(Bytes::from_static(b"]}"))).await;
                    return;
                }
                // Too late for a status: the client sees the body cut short.
                Err(e) => {
                    tracing::error!("storage: {e}");
                    Err(std::io::Error::other("storage unavailable"))
                }
            };
            let failed = chunk.is_err();
            if tx.send(chunk).await.is_err() || failed {
                return;
            }
        }
    });
    let body = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|chunk| (chunk, rx))
    });
    (
        [(header::CONTENT_TYPE, "application/json")],
        Body::from_stream(body),
    )
        .into_response()
}

#[derive(Deserialize)]
struct StatsParams {
    days: Option<u32>,
}

async fn tool_stats(
    State(s): State<AppState>,
    viewer: Viewer,
    params: Result<Query<StatsParams>, QueryRejection>,
) -> Result<Json<ToolStats>, ApiError> {
    let Query(params) = params?;
    let days = params.days.unwrap_or(14).clamp(1, 365);
    let since = Utc::now() - chrono::Duration::days(i64::from(days));
    let (scope, _turn) = s.read(&viewer).await?;
    let tools = stats::tool_latency(&scope, since).await?;
    Ok(Json(ToolStats { days, since, tools }))
}
