//! Ground Station hosted API: serves the built UI and the same `/v1` read
//! contract gsd serves locally, scoped to the signed-in tenant, plus `/auth/*`
//! for signing in. Migrates both databases at startup.

pub mod accounts;
pub mod auth;
pub mod config;
mod error;
mod insights;
pub mod queries;
pub mod sessions;
mod tokens;
mod v1;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use axum::Router;
use axum::extract::State;
use axum::middleware;
use axum::response::IntoResponse;
use axum::routing::get;
use groundstation_backend::ch::{Clickhouse, TenantScope, schema};
use groundstation_backend::config::Service;
use groundstation_backend::pg::{self, Pool};
use groundstation_backend::{health, service};

use crate::accounts::Signup;
use crate::auth::oidc::Oidc;
use crate::auth::{Throttle, Viewer};
pub use crate::config::Config;
use crate::error::ApiError;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub clickhouse: Clickhouse,
    pub pg: Pool,
    oidc: Option<Arc<Oidc>>,
    password_attempts: Arc<Throttle>,
    /// Per tenant: its counts and when they were taken, behind a lock that
    /// requests missing them at once wait on, so they count only once.
    counts: Arc<Mutex<HashMap<uuid::Uuid, CountsSlot>>>,
    /// Per tenant: reads of ClickHouse running at once.
    reads: Arc<Mutex<HashMap<uuid::Uuid, Arc<Semaphore>>>>,
}

/// A tenant's trajectories and events.
type Counts = (u64, u64);
/// A tenant's counts and when they were taken.
type CountsSlot = Arc<tokio::sync::Mutex<Option<(Counts, Instant)>>>;

/// How long a tenant's counts are reused. Counting reads every event id the
/// tenant has, and every open tab asks every ten seconds.
const COUNTS_TTL: Duration = Duration::from_secs(30);

/// Reads one tenant may run at once on this instance, so no tenant takes the
/// ClickHouse every tenant shares; and how long a read waits for its turn.
const READS_PER_TENANT: usize = 4;
const READ_WAIT: Duration = Duration::from_secs(10);

/// Tenants remembered before idle ones are forgotten.
const TENANTS_KEPT: usize = 10_000;

impl AppState {
    pub fn new(mut config: Config) -> anyhow::Result<Self> {
        config.check()?;
        Ok(Self {
            clickhouse: Clickhouse::connect(&config.clickhouse, Service::Api),
            pg: pg::connect(&config.postgres)?,
            oidc: Oidc::new(&config.sign_in, &config.public_url)?.map(Arc::new),
            password_attempts: Arc::default(),
            counts: Arc::default(),
            reads: Arc::default(),
            config: Arc::new(config),
        })
    }

    /// How many trajectories and events the viewer's tenant has, at most
    /// [`COUNTS_TTL`] old. Counting takes one of the tenant's read turns only
    /// when the cached counts are too old, and with none free, older counts
    /// beat an error: health must answer while the tenant reads a lot.
    async fn counts(&self, viewer: &Viewer) -> Result<Counts, ApiError> {
        let slot = {
            let mut all = self.counts.lock().unwrap_or_else(|e| e.into_inner());
            if all.len() >= TENANTS_KEPT {
                all.retain(|_, slot| Arc::strong_count(slot) > 1);
            }
            all.entry(viewer.tenant_id).or_default().clone()
        };
        let mut cached = slot.lock().await;
        if let Some((counts, at)) = *cached
            && at.elapsed() < COUNTS_TTL
        {
            return Ok(counts);
        }
        let (scope, _turn) = match *cached {
            // Counts to fall back on: take a turn only if one is free now.
            Some((older, _)) => match self.try_read(viewer) {
                Some(read) => read,
                None => return Ok(older),
            },
            None => self.read(viewer).await?,
        };
        let counts = queries::trajectories::counts(&scope).await?;
        *cached = Some((counts, Instant::now()));
        Ok(counts)
    }

    /// Reads limited to the viewer's tenant, and a turn to run them: one of
    /// [`READS_PER_TENANT`], held until the permit is dropped.
    async fn read(&self, viewer: &Viewer) -> Result<(TenantScope, OwnedSemaphorePermit), ApiError> {
        let turns = turns_for(&self.reads, viewer.tenant_id, TENANTS_KEPT);
        let busy = || {
            ApiError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "too many reads at once, retry shortly",
            )
        };
        let turn = tokio::time::timeout(READ_WAIT, turns.acquire_owned())
            .await
            .map_err(|_| busy())?
            .map_err(|_| busy())?;
        Ok((self.scope(viewer), turn))
    }

    /// Like [`Self::read`], without waiting: `None` if every turn is taken.
    fn try_read(&self, viewer: &Viewer) -> Option<(TenantScope, OwnedSemaphorePermit)> {
        let turns = turns_for(&self.reads, viewer.tenant_id, TENANTS_KEPT);
        let turn = turns.try_acquire_owned().ok()?;
        Some((self.scope(viewer), turn))
    }

    /// Reads limited to the viewer's tenant.
    fn scope(&self, viewer: &Viewer) -> TenantScope {
        self.clickhouse.tenant(viewer.tenant_id)
    }

    fn signup(&self) -> Signup {
        Signup {
            new_users: self.config.sign_in.new_users,
            retention_days: self.config.retention_days,
        }
    }
}

/// A tenant's read turns. Past `kept` tenants, those no request is using are
/// forgotten.
fn turns_for(
    reads: &Mutex<HashMap<uuid::Uuid, Arc<Semaphore>>>,
    tenant: uuid::Uuid,
    kept: usize,
) -> Arc<Semaphore> {
    let mut all = reads.lock().unwrap_or_else(|e| e.into_inner());
    if all.len() >= kept {
        // A request holds a clone from before it waits for a turn until after
        // it gives the turn back: forgetting those would give the tenant a
        // second set of turns.
        all.retain(|_, turns| Arc::strong_count(turns) > 1);
    }
    all.entry(tenant)
        .or_insert_with(|| Arc::new(Semaphore::new(READS_PER_TENANT)))
        .clone()
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(v1::routes())
        .merge(tokens::routes())
        .merge(insights::routes())
        .merge(auth::routes())
        .route("/healthz", get(healthz))
        .fallback(groundstation_ui_embed::serve)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::same_origin,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::https_only,
        ))
        .layer(middleware::from_fn(groundstation_ui_embed::no_store))
        .with_state(state)
}

async fn healthz(State(s): State<AppState>) -> impl IntoResponse {
    health::report(&s.clickhouse, &s.pg).await
}

/// Brings both databases up to this version's schema. Safe to run from
/// several instances at once.
pub async fn migrate(config: &Config) -> anyhow::Result<()> {
    let pool = pg::connect(&config.postgres)?;
    pg::migrate(&pool).await?;
    let admin = Clickhouse::connect_admin(&config.clickhouse, &config.clickhouse_admin);
    schema::migrate(&admin).await?;
    Ok(())
}

pub async fn run(config: Config) -> anyhow::Result<()> {
    let state = AppState::new(config)?;
    migrate(&state.config).await?;
    let listen = state.config.listen;
    service::serve("groundstation-api", listen, router(state)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use uuid::Uuid;

    use super::*;

    fn state() -> AppState {
        AppState::new(Config::parse_from([
            "api",
            "--admin-password",
            "a long enough password",
        ]))
        .unwrap()
    }

    #[tokio::test]
    async fn stale_counts_answer_at_once_while_every_read_turn_is_taken() {
        let state = state();
        let viewer = Viewer {
            user_id: Uuid::now_v7(),
            tenant_id: Uuid::now_v7(),
            admin: true,
        };
        let stale = Instant::now() - COUNTS_TTL - Duration::from_secs(1);
        state.counts.lock().unwrap().insert(
            viewer.tenant_id,
            Arc::new(tokio::sync::Mutex::new(Some(((5, 7), stale)))),
        );
        let _every_turn = turns_for(&state.reads, viewer.tenant_id, TENANTS_KEPT)
            .acquire_many_owned(READS_PER_TENANT as u32)
            .await
            .unwrap();
        let started = Instant::now();
        assert_eq!(state.counts(&viewer).await.unwrap(), (5, 7));
        let waited = started.elapsed();
        assert!(waited < Duration::from_secs(1), "health waited {waited:?}");
    }

    #[test]
    fn turns_a_request_holds_are_never_forgotten() {
        let reads = Mutex::new(HashMap::new());
        let (busy, idle) = (Uuid::now_v7(), Uuid::now_v7());
        // A request has the tenant's turns and hasn't taken one yet.
        let held = turns_for(&reads, busy, 2);
        drop(turns_for(&reads, idle, 2));
        // Two tenants kept: the next one forgets those nobody uses.
        drop(turns_for(&reads, Uuid::now_v7(), 2));
        assert!(!reads.lock().unwrap().contains_key(&idle));
        assert!(
            Arc::ptr_eq(&held, &turns_for(&reads, busy, 3)),
            "the tenant got a second set of turns"
        );
    }
}
