//! Helpers for tests against the databases from `docker-compose.yml`. Each
//! test works in a fresh tenant, so tests don't see each other's data and need
//! no cleanup.

use chrono::{DateTime, Utc};
use clap::Parser;
use groundstation_schema::{Agent, Event, EventKind};
use uuid::Uuid;

use crate::ch::{Clickhouse, Stamp};
use crate::config::{ClickhouseAdminArgs, ClickhouseArgs, PostgresArgs, Service};
use crate::pg::{self, Pool, tenants::Tenant};

/// The settings every service starts with, environment variables included.
#[derive(Parser)]
struct Defaults {
    #[command(flatten)]
    clickhouse: ClickhouseArgs,
    #[command(flatten)]
    admin: ClickhouseAdminArgs,
    #[command(flatten)]
    postgres: PostgresArgs,
}

pub struct Databases {
    pub pg: Pool,
    pub admin: Clickhouse,
    pub ingest: Clickhouse,
    pub api: Clickhouse,
    pub clickhouse_args: ClickhouseArgs,
    pub postgres_args: PostgresArgs,
}

/// Connects as every service and migrates both databases.
pub async fn databases() -> Databases {
    let d = Defaults::parse_from(["test"]);
    let pg = pg::connect(&d.postgres).expect("postgres settings");
    pg::migrate(&pg).await.expect("migrating postgres");
    let admin = Clickhouse::connect_admin(&d.clickhouse, &d.admin);
    crate::ch::schema::migrate(&admin)
        .await
        .expect("migrating clickhouse");
    Databases {
        pg,
        ingest: Clickhouse::connect(&d.clickhouse, Service::Ingest),
        api: Clickhouse::connect(&d.clickhouse, Service::Api),
        admin,
        clickhouse_args: d.clickhouse,
        postgres_args: d.postgres,
    }
}

pub async fn tenant(pg: &Pool) -> Tenant {
    pg::tenants::create(pg, &format!("test-{}", Uuid::now_v7()), 30)
        .await
        .expect("creating a tenant")
}

/// What ingest would stamp on a request from `tenant` now.
pub fn stamp(tenant: &Tenant) -> Stamp {
    Stamp::new(tenant.id, Uuid::now_v7(), tenant.retention_days, Utc::now())
}

pub fn event(trajectory: &str, kind: EventKind, at: DateTime<Utc>) -> Event {
    Event::new(Uuid::now_v7(), trajectory, kind, at, Agent::named("test"))
}
