//! Tenants: the unit of isolation. Every event, token and insight belongs to one.

use chrono::{DateTime, Utc};
use tokio_postgres::Row;
use uuid::Uuid;

use deadpool_postgres::GenericClient;

use super::Pool;
use crate::Result;

#[derive(Debug, Clone, PartialEq)]
pub struct Tenant {
    pub id: Uuid,
    pub name: String,
    pub plan: String,
    /// How long events are kept, counted from when they arrive.
    pub retention_days: i32,
    pub created_at: DateTime<Utc>,
    pub suspended_at: Option<DateTime<Utc>>,
}

const COLUMNS: &str = "id, name, plan, retention_days, created_at, suspended_at";

impl From<&Row> for Tenant {
    fn from(row: &Row) -> Self {
        Self {
            id: row.get("id"),
            name: row.get("name"),
            plan: row.get("plan"),
            retention_days: row.get("retention_days"),
            created_at: row.get("created_at"),
            suspended_at: row.get("suspended_at"),
        }
    }
}

pub async fn create(pool: &Pool, name: &str, retention_days: i32) -> Result<Tenant> {
    insert(&pool.get().await?, name, retention_days).await
}

/// [`create`] with a connection or transaction the caller holds.
pub async fn insert(
    client: &impl GenericClient,
    name: &str,
    retention_days: i32,
) -> Result<Tenant> {
    let row = client
        .query_one(
            &format!(
                "INSERT INTO tenants (id, name, retention_days) VALUES ($1, $2, $3)
                 RETURNING {COLUMNS}"
            ),
            &[&Uuid::now_v7(), &name, &retention_days],
        )
        .await?;
    Ok(Tenant::from(&row))
}

/// Creates the tenant with this fixed `id` unless it exists. Returns whether
/// this call created it.
pub async fn insert_fixed(
    client: &impl GenericClient,
    id: Uuid,
    name: &str,
    retention_days: i32,
) -> Result<bool> {
    let created = client
        .execute(
            "INSERT INTO tenants (id, name, retention_days) VALUES ($1, $2, $3)
             ON CONFLICT (id) DO NOTHING",
            &[&id, &name, &retention_days],
        )
        .await?;
    Ok(created == 1)
}

pub async fn get(pool: &Pool, id: Uuid) -> Result<Option<Tenant>> {
    let row = pool
        .get()
        .await?
        .query_opt(
            &format!("SELECT {COLUMNS} FROM tenants WHERE id = $1"),
            &[&id],
        )
        .await?;
    Ok(row.as_ref().map(Tenant::from))
}

/// Tenants that aren't suspended, oldest first.
pub async fn active(pool: &Pool) -> Result<Vec<Tenant>> {
    let rows = pool
        .get()
        .await?
        .query(
            &format!(
                "SELECT {COLUMNS} FROM tenants WHERE suspended_at IS NULL ORDER BY created_at"
            ),
            &[],
        )
        .await?;
    Ok(rows.iter().map(Tenant::from).collect())
}

pub async fn set_suspended(pool: &Pool, id: Uuid, suspended: bool) -> Result<()> {
    pool.get()
        .await?
        .execute(
            "UPDATE tenants SET suspended_at = CASE WHEN $2 THEN now() END WHERE id = $1",
            &[&id, &suspended],
        )
        .await?;
    Ok(())
}
