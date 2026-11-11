//! `/v1/insights`: what the worker found in the tenant's events, and triage.

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use groundstation_backend::pg::insights;
use groundstation_wire::insights::{Insight, InsightState, StateChange};
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::auth::Viewer;
use crate::error::ApiError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/insights", get(list))
        .route("/v1/insights/{id}", get(one))
        .route("/v1/insights/{id}/state", post(set_state))
}

#[derive(Deserialize)]
struct ListParams {
    /// `open` (the default), `dismissed`, `resolved`, or `all`.
    state: Option<String>,
    limit: Option<i64>,
}

async fn list(
    State(s): State<AppState>,
    viewer: Viewer,
    q: Result<Query<ListParams>, QueryRejection>,
) -> Result<Json<Vec<Insight>>, ApiError> {
    let Query(q) = q?;
    let state = match q.state.as_deref().unwrap_or("open") {
        "all" => None,
        "open" => Some(InsightState::Open),
        "dismissed" => Some(InsightState::Dismissed),
        "resolved" => Some(InsightState::Resolved),
        // Never echo what was sent: errors are for people, and can be long.
        _ => {
            return Err(ApiError::new(
                axum::http::StatusCode::BAD_REQUEST,
                "unknown state; use open, dismissed, resolved or all",
            ));
        }
    };
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    Ok(Json(
        insights::list(&s.pg, viewer.tenant_id, state, limit).await?,
    ))
}

async fn one(
    State(s): State<AppState>,
    viewer: Viewer,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<Json<Insight>, ApiError> {
    let Path(id) = id.map_err(|_| ApiError::not_found("finding"))?;
    insights::get(&s.pg, viewer.tenant_id, id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("finding"))
}

async fn set_state(
    State(s): State<AppState>,
    viewer: Viewer,
    id: Result<Path<Uuid>, PathRejection>,
    change: Result<Json<StateChange>, JsonRejection>,
) -> Result<Json<Insight>, ApiError> {
    let Path(id) = id.map_err(|_| ApiError::not_found("finding"))?;
    let Json(change) = change?;
    if !insights::set_state(&s.pg, viewer.tenant_id, id, change.state, viewer.user_id).await? {
        return Err(ApiError::not_found("finding"));
    }
    one(State(s), viewer, Ok(Path(id))).await
}
