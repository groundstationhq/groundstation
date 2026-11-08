//! Ingest tokens: what `gsd` sends as its bearer token. A token can only write
//! events into its tenant, never read. Only its hash is stored; the token
//! itself is shown once, when it's created.

use chrono::{DateTime, Utc};
use deadpool_postgres::GenericClient;
use tokio_postgres::Row;
use uuid::Uuid;

use super::Pool;
use crate::{Result, secret};

/// What a valid token lets a request do.
#[derive(Debug, Clone, PartialEq)]
pub struct Grant {
    pub token_id: Uuid,
    pub tenant_id: Uuid,
    pub retention_days: i32,
    pub suspended: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IngestToken {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub label: String,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

const COLUMNS: &str = "id, tenant_id, label, created_by, created_at, last_used_at, revoked_at";

impl From<&Row> for IngestToken {
    fn from(row: &Row) -> Self {
        Self {
            id: row.get("id"),
            tenant_id: row.get("tenant_id"),
            label: row.get("label"),
            created_by: row.get("created_by"),
            created_at: row.get("created_at"),
            last_used_at: row.get("last_used_at"),
            revoked_at: row.get("revoked_at"),
        }
    }
}

/// Resolves a presented token. Records the use; callers cache the grant, so
/// `last_used_at` moves at most once per cache period.
pub async fn authenticate(pool: &Pool, token: &str) -> Result<Option<Grant>> {
    let row = pool
        .get()
        .await?
        .query_opt(
            "UPDATE ingest_tokens t SET last_used_at = now()
             FROM tenants n
             WHERE t.token_hash = $1 AND t.revoked_at IS NULL AND n.id = t.tenant_id
             RETURNING t.id, t.tenant_id, n.retention_days, n.suspended_at IS NOT NULL",
            &[&secret::hash(token)],
        )
        .await?;
    Ok(row.map(|r| Grant {
        token_id: r.get(0),
        tenant_id: r.get(1),
        retention_days: r.get(2),
        suspended: r.get(3),
    }))
}

/// Creates a token and returns it with the secret, which isn't stored.
pub async fn create(
    pool: &Pool,
    tenant_id: Uuid,
    label: &str,
    created_by: Option<Uuid>,
) -> Result<(IngestToken, String)> {
    insert(&pool.get().await?, tenant_id, label, created_by).await
}

/// [`create`] with a connection or transaction the caller holds.
pub async fn insert(
    client: &impl GenericClient,
    tenant_id: Uuid,
    label: &str,
    created_by: Option<Uuid>,
) -> Result<(IngestToken, String)> {
    let token = secret::generate(secret::INGEST_TOKEN_PREFIX);
    let row = client
        .query_one(
            &format!(
                "INSERT INTO ingest_tokens (id, tenant_id, token_hash, label, created_by)
                 VALUES ($1, $2, $3, $4, $5) RETURNING {COLUMNS}"
            ),
            &[
                &Uuid::now_v7(),
                &tenant_id,
                &secret::hash(&token),
                &label,
                &created_by,
            ],
        )
        .await?;
    Ok((IngestToken::from(&row), token))
}

/// A tenant's tokens, newest first, revoked ones included.
pub async fn list(pool: &Pool, tenant_id: Uuid) -> Result<Vec<IngestToken>> {
    let rows = pool
        .get()
        .await?
        .query(
            &format!(
                "SELECT {COLUMNS} FROM ingest_tokens WHERE tenant_id = $1 ORDER BY created_at DESC"
            ),
            &[&tenant_id],
        )
        .await?;
    Ok(rows.iter().map(IngestToken::from).collect())
}

pub async fn get(pool: &Pool, tenant_id: Uuid, id: Uuid) -> Result<Option<IngestToken>> {
    let row = pool
        .get()
        .await?
        .query_opt(
            &format!("SELECT {COLUMNS} FROM ingest_tokens WHERE tenant_id = $1 AND id = $2"),
            &[&tenant_id, &id],
        )
        .await?;
    Ok(row.as_ref().map(IngestToken::from))
}

/// Revokes one of the tenant's tokens. Returns false if it has no such token.
pub async fn revoke(pool: &Pool, tenant_id: Uuid, id: Uuid) -> Result<bool> {
    let changed = pool
        .get()
        .await?
        .execute(
            "UPDATE ingest_tokens SET revoked_at = COALESCE(revoked_at, now())
             WHERE tenant_id = $1 AND id = $2",
            &[&tenant_id, &id],
        )
        .await?;
    Ok(changed == 1)
}
