//! ClickHouse: where events live.
//!
//! Writes go through [`Clickhouse::insert_events`]. Reads go through a
//! [`TenantScope`], which binds the tenant to every query and sets the
//! `SQL_tenant_id` setting the row policy checks, so a read can only ever see
//! one tenant's rows.

mod events;
pub mod schema;

use std::time::Duration;

pub use clickhouse::Row;
pub use clickhouse::query::Query;
pub use events::{EventRow, Stamp};
use uuid::Uuid;

use crate::config::{ClickhouseAdminArgs, ClickhouseArgs, Service};
use crate::{Error, Result};

/// The placeholder every tenant-scoped query must filter with.
pub const TENANT: &str = "{tenant:UUID}";

/// What one tenant's read may take of the ClickHouse every tenant shares:
/// seconds, and bytes of memory (sorts and groupings past half of that go to
/// disk instead). A query past either fails, and the request with it.
pub const READ_SECONDS: u64 = 30;
pub const READ_MEMORY: u64 = 2 << 30;

/// Seconds for a read that goes at its reader's pace, not ClickHouse's
/// ([`TenantScope::unhurried`]). ClickHouse counts the time it waits for the
/// reader to take rows.
pub const UNHURRIED_SECONDS: u64 = 600;

#[derive(Clone)]
pub struct Clickhouse {
    client: clickhouse::Client,
    database: String,
}

impl Clickhouse {
    /// Connects as `service`'s own user. Lazy: nothing is sent until the
    /// first query.
    pub fn connect(args: &ClickhouseArgs, service: Service) -> Self {
        Self::with_credentials(args, args.user_for(service), &args.clickhouse_password)
    }

    /// Connects as the admin user, for migrations.
    pub fn connect_admin(args: &ClickhouseArgs, admin: &ClickhouseAdminArgs) -> Self {
        Self::with_credentials(
            args,
            &admin.clickhouse_admin_user,
            &admin.clickhouse_admin_password,
        )
    }

    fn with_credentials(args: &ClickhouseArgs, user: &str, password: &str) -> Self {
        let client = clickhouse::Client::default()
            .with_url(&args.clickhouse_url)
            .with_user(user)
            .with_password(password)
            .with_database(&args.clickhouse_database);
        Self {
            client,
            database: args.clickhouse_database.clone(),
        }
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    /// Checks that ClickHouse answers and accepts the credentials.
    pub async fn ping(&self) -> Result<()> {
        self.client.query("SELECT 1").fetch_one::<u8>().await?;
        Ok(())
    }

    /// Reads as `tenant`.
    pub fn tenant(&self, tenant: Uuid) -> TenantScope {
        TenantScope {
            client: self.client.clone(),
            tenant,
            seconds: READ_SECONDS,
        }
    }

    /// Inserts rows and returns once ClickHouse has committed them. Async
    /// inserts let ClickHouse batch many small requests into few parts.
    pub async fn insert_events(&self, rows: &[EventRow]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut insert = self
            .client
            .insert::<EventRow>("events")?
            .with_option("async_insert", "1")
            .with_option("wait_for_async_insert", "1")
            .with_timeouts(Some(Duration::from_secs(10)), Some(Duration::from_secs(30)));
        for row in rows {
            insert.write(row).await?;
        }
        insert.end().await?;
        Ok(())
    }

    /// Runs one statement as is. For migrations and admin operations only:
    /// reads of tenant data go through [`TenantScope`].
    pub async fn execute(&self, sql: &str) -> Result<()> {
        self.client.query(sql).execute().await?;
        Ok(())
    }
}

/// The tenant's events matching `filter`, with `columns`: the newest copy of
/// each. Every read of `events` goes through this: a redelivered or updated
/// event is stored once per upload until ClickHouse merges the copies. `FINAL`
/// has ClickHouse merge them as it reads, by the table's own rule (the copy
/// with the latest `received_at` wins, as the newest upload does in gsd), and
/// `filter` then sees only that copy. Materialized columns must be named: `*`
/// leaves them out.
pub fn deduplicated(columns: &str, filter: &str) -> String {
    format!(
        "SELECT {columns} FROM events FINAL
         WHERE tenant_id = {{tenant:UUID}} AND ({filter})"
    )
}

/// Reads limited to one tenant. Every query must contain [`TENANT`] in its
/// filter; the tenant is bound as a parameter, never formatted into SQL.
#[derive(Clone)]
pub struct TenantScope {
    client: clickhouse::Client,
    tenant: Uuid,
    /// What each query may take.
    seconds: u64,
}

impl TenantScope {
    pub fn tenant_id(&self) -> Uuid {
        self.tenant
    }

    /// The same reads, each allowed [`UNHURRIED_SECONDS`] and no more memory.
    /// For a timeline streamed to a browser, which downloads it as fast as
    /// its connection does, and for the worker's passes over everything a
    /// tenant uploaded in a week, which nobody is waiting for.
    pub fn unhurried(&self) -> Self {
        Self {
            seconds: UNHURRIED_SECONDS,
            ..self.clone()
        }
    }

    pub fn query(&self, sql: &str) -> Result<Query> {
        if !sql.contains(TENANT) {
            return Err(Error::Unscoped);
        }
        let tenant = self.tenant.to_string();
        let spill = (READ_MEMORY / 2).to_string();
        Ok(self
            .client
            .query(sql)
            .param("tenant", &tenant)
            .with_option("SQL_tenant_id", tenant)
            // Copies of an event uploaded in different months are in
            // different partitions: `FINAL` must merge across them.
            .with_option("do_not_merge_across_partitions_select_final", "0")
            .with_option("max_execution_time", self.seconds.to_string())
            .with_option("max_memory_usage", READ_MEMORY.to_string())
            .with_option("max_bytes_before_external_group_by", spill.clone())
            .with_option("max_bytes_before_external_sort", spill))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unscoped_reads_are_refused() {
        let ch = Clickhouse {
            client: clickhouse::Client::default(),
            database: "db".into(),
        };
        let scope = ch.tenant(Uuid::nil());
        assert!(matches!(
            scope.query("SELECT count() FROM events"),
            Err(Error::Unscoped)
        ));
        assert!(
            scope
                .query("SELECT count() FROM events WHERE tenant_id = {tenant:UUID}")
                .is_ok()
        );
    }
}
