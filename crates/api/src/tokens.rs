//! `/v1/tokens`: the ingest tokens `gsd` uploads with. Any member can list
//! them and create one for a machine; a token can be revoked by whoever
//! created it or by a tenant admin.

use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get};
use axum::{Json, Router};
use deadpool_postgres::GenericClient;
use groundstation_backend::pg::tokens;
use groundstation_wire::{CreatedToken, IngestToken, NewToken};
use uuid::Uuid;

use crate::AppState;
use crate::auth::Viewer;
use crate::error::ApiError;

pub const MAX_LABEL: usize = groundstation_wire::MAX_TOKEN_LABEL;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/tokens", get(list).post(create))
        .route("/v1/tokens/{id}", delete(revoke))
}

pub fn wire(t: &tokens::IngestToken) -> IngestToken {
    IngestToken {
        id: t.id,
        label: t.label.clone(),
        created_at: t.created_at,
        last_used_at: t.last_used_at,
        revoked_at: t.revoked_at,
    }
}

async fn list(
    State(s): State<AppState>,
    viewer: Viewer,
) -> Result<Json<Vec<IngestToken>>, ApiError> {
    let all = tokens::list(&s.pg, viewer.tenant_id).await?;
    Ok(Json(all.iter().map(wire).collect()))
}

async fn create(
    State(s): State<AppState>,
    viewer: Viewer,
    new: Result<Json<NewToken>, JsonRejection>,
) -> Result<(StatusCode, Json<CreatedToken>), ApiError> {
    let Json(new) = new?;
    let label = check_label(&new.label)?;
    let created = issue(&s, &viewer, label).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// Creates a token for the viewer's tenant. Also used by the CLI sign-in page.
pub async fn issue(s: &AppState, viewer: &Viewer, label: &str) -> Result<CreatedToken, ApiError> {
    let conn =
        s.pg.get()
            .await
            .map_err(groundstation_backend::Error::from)?;
    let created = issue_with(&conn, viewer, label).await?;
    tracing::info!(tenant = %viewer.tenant_id, token = %created.token.id, "created ingest token");
    Ok(created)
}

/// [`issue`] with a connection or transaction the caller holds, and logs
/// once it commits.
pub async fn issue_with(
    client: &impl GenericClient,
    viewer: &Viewer,
    label: &str,
) -> Result<CreatedToken, ApiError> {
    let (token, secret) =
        tokens::insert(client, viewer.tenant_id, label, Some(viewer.user_id)).await?;
    Ok(CreatedToken {
        token: wire(&token),
        secret,
    })
}

pub fn check_label(label: &str) -> Result<&str, ApiError> {
    if !groundstation_wire::valid_token_label(label) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "label must be 1 to {MAX_LABEL} characters, none of them control or invisible characters"
            ),
        ));
    }
    Ok(label.trim())
}

async fn revoke(
    State(s): State<AppState>,
    viewer: Viewer,
    id: Result<Path<Uuid>, PathRejection>,
) -> Result<StatusCode, ApiError> {
    let Path(id) = id.map_err(|_| ApiError::not_found("token"))?;
    let token = tokens::get(&s.pg, viewer.tenant_id, id)
        .await?
        .ok_or_else(|| ApiError::not_found("token"))?;
    if !viewer.admin && token.created_by != Some(viewer.user_id) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "only its creator or an admin can revoke this token",
        ));
    }
    tokens::revoke(&s.pg, viewer.tenant_id, id).await?;
    tracing::info!(tenant = %viewer.tenant_id, token = %id, "revoked ingest token");
    Ok(StatusCode::NO_CONTENT)
}
