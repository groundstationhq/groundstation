//! Ground Station insights worker. Every few minutes it looks at each tenant
//! with events newer than it has seen, runs the detectors over the tenant's
//! recent window, and stores what they find in Postgres for the api to serve.
//!
//! Open findings are what the detectors see in that window now: one a
//! detector no longer finds is closed, and one whose events its tenant no
//! longer keeps is deleted.

pub mod detectors;
mod format;

use std::future::Future;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use clickhouse::Row;
use groundstation_backend::ch::{Clickhouse, TenantScope};
use groundstation_backend::config::{ClickhouseArgs, PostgresArgs, Service};
use groundstation_backend::pg::tenants::Tenant;
use groundstation_backend::pg::{self, Pool, insights, tenants};
use groundstation_backend::service;
use serde::Deserialize;
use tracing::Instrument;

use crate::detectors::{Detector, Window};

#[derive(clap::Parser, Debug)]
#[command(version, about = "Ground Station insights worker")]
pub struct Config {
    /// Seconds between passes over all tenants.
    #[arg(long, env = "GROUNDSTATION_WORKER_INTERVAL", default_value_t = 300)]
    pub interval: u64,
    /// How many days of events each detector looks at (1 to 365), at most as
    /// many as the tenant keeps.
    #[arg(
        long,
        env = "GROUNDSTATION_WORKER_WINDOW_DAYS",
        default_value_t = 7,
        value_parser = clap::value_parser!(i64).range(1..=365)
    )]
    pub window_days: i64,
    /// Make one pass and exit, e.g. from a scheduler. Exits non-zero if any
    /// part of the pass failed.
    #[arg(long)]
    pub once: bool,
    /// How long after ingest stamps a batch it may still be committing, plus
    /// how far ingest instances' clocks may differ. Events stamped this close
    /// to a run are looked at again in the next one.
    #[arg(long, hide = true, default_value_t = 120)]
    pub settle_secs: u64,
    #[command(flatten)]
    pub clickhouse: ClickhouseArgs,
    #[command(flatten)]
    pub postgres: PostgresArgs,
}

pub struct Worker {
    clickhouse: Clickhouse,
    pg: Pool,
    window_days: i64,
    settle: chrono::Duration,
}

/// What a pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Pass {
    /// Findings stored or refreshed.
    pub stored: usize,
    /// Tenants, detectors and sweeps that failed and will be tried again.
    pub failed: usize,
}

impl Worker {
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        // One connection holds the tenant's lock while others write.
        let postgres = PostgresArgs {
            postgres_max_connections: config.postgres.postgres_max_connections.max(2),
            ..config.postgres.clone()
        };
        Ok(Self {
            clickhouse: Clickhouse::connect(&config.clickhouse, Service::Worker),
            pg: pg::connect(&postgres)?,
            window_days: config.window_days,
            settle: chrono::Duration::seconds(
                i64::try_from(config.settle_secs).unwrap_or(i64::MAX),
            ),
        })
    }

    /// Closes and deletes findings that have aged out, then runs every
    /// detector that has events it hasn't seen, for every active tenant. One
    /// tenant's or detector's failure doesn't stop the others, and neither
    /// holds up the sweep, which goes first.
    pub async fn pass(&self) -> anyhow::Result<Pass> {
        let mut pass = Pass::default();
        let window_days = i32::try_from(self.window_days).unwrap_or(i32::MAX);
        match insights::sweep(&self.pg, window_days).await {
            Ok((closed, deleted)) if closed + deleted > 0 => {
                tracing::info!(
                    closed,
                    deleted,
                    "swept findings past their window or events"
                );
            }
            Ok(_) => {}
            Err(e) => {
                pass.failed += 1;
                tracing::error!("sweeping findings failed: {e:#}");
            }
        }
        for tenant in tenants::active(&self.pg).await? {
            let span = tracing::info_span!("tenant", tenant = %tenant.id);
            match self.pass_tenant(&tenant).instrument(span).await {
                Ok(t) => {
                    pass.stored += t.stored;
                    pass.failed += t.failed;
                }
                Err(e) => {
                    pass.failed += 1;
                    tracing::error!(tenant = %tenant.id, "insights pass failed: {e:#}");
                }
            }
        }
        Ok(pass)
    }

    /// One tenant's turn in a pass, while no other worker takes it: two
    /// instances (a rolling deploy, a scheduled `--once`) would otherwise
    /// write findings from runs of different ages over each other.
    pub async fn pass_tenant(&self, tenant: &Tenant) -> anyhow::Result<Pass> {
        let key = format!("groundstation-worker:{}", tenant.id);
        let lock = self.pg.get().await?;
        lock.execute("SELECT pg_advisory_lock(hashtextextended($1, 0))", &[&key])
            .await?;
        let result = self.detect(tenant).await;
        if let Err(e) = lock
            .execute(
                "SELECT pg_advisory_unlock(hashtextextended($1, 0))",
                &[&key],
            )
            .await
        {
            // Closing the connection lets the lock go.
            tracing::warn!("couldn't release the tenant's lock: {e}");
            drop(pg::Connection::take(lock));
        }
        result
    }

    async fn detect(&self, tenant: &Tenant) -> anyhow::Result<Pass> {
        // Nobody waits for a pass: a tenant with a lot to read gets the time.
        let scope = self.clickhouse.tenant(tenant.id).unhurried();
        let Some(latest) = latest_upload(&scope).await? else {
            return Ok(Pass::default());
        };
        let now = Utc::now();
        // Events past the tenant's retention are on their way out.
        let days = self
            .window_days
            .min(i64::from(tenant.retention_days))
            .max(1);
        let window = Window::ending(now, days);
        // Ingest stamps a batch before it commits, and batches can commit out
        // of order: what this run records as seen stops short of the events
        // stamped just before it, so the next run looks at them again.
        let seen_now = latest.min(now - self.settle);
        let mut pass = Pass::default();
        for detector in Detector::ALL {
            let name = detector.name();
            let seen = insights::watermark(&self.pg, tenant.id, name).await?;
            if seen.is_some_and(|seen| seen >= latest) {
                continue;
            }
            let started = Instant::now();
            let found = match detector.run(&scope, window).await {
                Ok(found) => found,
                Err(e) => {
                    pass.failed += 1;
                    tracing::error!(detector = name, "detector failed: {e}");
                    continue;
                }
            };
            if let Err(e) = insights::store(&self.pg, tenant.id, &found.findings, now).await {
                pass.failed += 1;
                tracing::error!(detector = name, "storing findings failed: {e:#}");
                continue;
            }
            // What this run didn't find again has stopped, or left the window.
            // Unless the run left findings out to stay under the cap: those
            // may still be there, so they stay as they are.
            let closed = if found.dropped == 0 {
                insights::resolve_unseen(&self.pg, tenant.id, name, now).await?
            } else {
                tracing::warn!(
                    detector = name,
                    dropped = found.dropped,
                    "kept the {} most severe findings and left the rest out",
                    detectors::MAX_FINDINGS
                );
                0
            };
            insights::record_run(&self.pg, tenant.id, name, seen_now).await?;
            pass.stored += found.findings.len();
            let ms = started.elapsed().as_millis() as u64;
            tracing::info!(
                detector = name,
                findings = found.findings.len(),
                closed,
                ms,
                "detector ran"
            );
        }
        Ok(pass)
    }
}

/// When the tenant's newest event arrived; `None` if it has none.
async fn latest_upload(
    scope: &TenantScope,
) -> groundstation_backend::Result<Option<DateTime<Utc>>> {
    #[derive(Row, Deserialize)]
    struct Latest {
        events: u64,
        #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
        at: DateTime<Utc>,
    }
    let latest: Latest = scope
        .query("SELECT count(), max(received_at) FROM events WHERE tenant_id = {tenant:UUID}")?
        .fetch_one()
        .await?;
    Ok((latest.events > 0).then_some(latest.at))
}

pub async fn run(config: Config) -> anyhow::Result<()> {
    run_until(config, service::shutdown_signal()).await
}

/// [`run`], until `shutdown` resolves, whether in a pass or between passes.
/// A pass cut short leaves nothing half written: each detector's findings
/// are one statement, and its watermark moves only after them.
pub async fn run_until(config: Config, shutdown: impl Future<Output = ()>) -> anyhow::Result<()> {
    let worker = Worker::new(&config)?;
    tracing::info!(
        interval = config.interval,
        window_days = config.window_days,
        "groundstation-worker {} ready",
        env!("CARGO_PKG_VERSION")
    );
    tokio::pin!(shutdown);
    loop {
        let pass = tokio::select! {
            pass = worker.pass() => pass,
            () = &mut shutdown => break,
        };
        if config.once {
            let pass = pass?;
            anyhow::ensure!(pass.failed == 0, "{} parts of the pass failed", pass.failed);
            return Ok(());
        }
        if let Err(e) = pass {
            tracing::error!("insights pass failed: {e:#}");
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(config.interval.max(1))) => {}
            () = &mut shutdown => break,
        }
    }
    tracing::info!("shutting down");
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn the_window_is_one_day_to_a_year() {
        for days in ["0", "-3", "366"] {
            assert!(
                Config::try_parse_from(["worker", "--window-days", days]).is_err(),
                "{days}"
            );
        }
        let config = Config::try_parse_from(["worker", "--window-days", "365"]).unwrap();
        assert_eq!(config.window_days, 365);
    }

    /// A Postgres that accepts connections and never answers.
    async fn silent_postgres() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        format!("postgres://u:p@127.0.0.1:{port}/db")
    }

    #[tokio::test]
    async fn a_stop_during_a_pass_ends_the_worker() {
        let url = silent_postgres().await;
        let config = Config::parse_from(["worker", "--postgres-url", &url, "--interval", "1"]);
        let stop = tokio::time::sleep(Duration::from_millis(300));
        let ran = tokio::time::timeout(Duration::from_secs(3), run_until(config, stop)).await;
        assert!(matches!(ran, Ok(Ok(()))), "{ran:?}");
    }

    #[tokio::test]
    async fn one_pass_that_fails_exits_with_the_error() {
        let config = Config::parse_from([
            "worker",
            "--once",
            "--postgres-url",
            "postgres://u:p@127.0.0.1:1/db",
        ]);
        let ran = run_until(config, std::future::pending()).await;
        assert!(ran.is_err());
    }
}
