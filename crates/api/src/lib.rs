//! Ground Station hosted API: serves the built UI and the same `/v1` read
//! contract gsd serves locally, scoped to the signed-in tenant, plus `/auth/*`
//! for signing in. Migrates both databases at startup.

pub mod accounts;
pub mod auth;
pub mod config;
mod error;
mod insights;
pub mod queries;
mod recent;
pub mod sessions;
mod tokens;
mod v1;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use axum::Router;
use axum::body::Bytes;
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
use crate::recent::{Busy, Recent};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub clickhouse: Clickhouse,
    pub pg: Pool,
    oidc: Option<Arc<Oidc>>,
    password_attempts: Arc<Throttle>,
    /// Per tenant and limit: the trajectory list as JSON, as last answered.
    lists: Arc<Recent<(uuid::Uuid, u32), Bytes>>,
    /// Per tenant: reads of ClickHouse running at once.
    reads: Arc<Mutex<HashMap<uuid::Uuid, Arc<Semaphore>>>>,
}

/// How long a tenant's trajectory list is reused. Listing summarizes every
/// event of the trajectories it names, and every open tab asks every five
/// seconds: within this long they all get the same answer, computed once.
const LIST_TTL: Duration = Duration::from_secs(5);

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
            lists: Arc::new(Recent::new(LIST_TTL, READ_WAIT)),
            reads: Arc::default(),
            config: Arc::new(config),
        })
    }

    /// The viewer's tenant's `limit` most recently updated trajectories, as
    /// the JSON `/v1/trajectories` answers with, at most [`LIST_TTL`] old.
    /// Only a request that finds none that recent takes one of the tenant's
    /// read turns; those arriving meanwhile wait for its answer, as long as
    /// they would wait for a turn.
    async fn list(&self, viewer: &Viewer, limit: u32) -> Result<Bytes, ApiError> {
        self.lists
            .get((viewer.tenant_id, limit), || async {
                let (scope, _turn) = self.read(viewer).await?;
                let list = queries::trajectories::list(&scope, limit).await?;
                Ok(serde_json::to_vec(&list)
                    .expect("summaries serialize")
                    .into())
            })
            .await
    }

    /// Reads limited to the viewer's tenant, and a turn to run them: one of
    /// [`READS_PER_TENANT`], held until the permit is dropped.
    async fn read(&self, viewer: &Viewer) -> Result<(TenantScope, OwnedSemaphorePermit), ApiError> {
        let turns = turns_for(&self.reads, viewer.tenant_id, TENANTS_KEPT);
        let turn = tokio::time::timeout(READ_WAIT, turns.acquire_owned())
            .await
            .map_err(|_| Busy)?
            .map_err(|_| Busy)?;
        Ok((self.scope(viewer), turn))
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

/// A read that waited [`READ_WAIT`] for its turn, or for another request's
/// answer, and got neither.
impl From<Busy> for ApiError {
    fn from(_: Busy) -> Self {
        Self::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "too many reads at once, retry shortly",
        )
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
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use clap::Parser;
    use groundstation_wire::{ENDPOINTS_PATH, Endpoints};
    use http_body_util::BodyExt;
    use uuid::Uuid;

    use super::*;

    #[tokio::test]
    async fn anyone_can_ask_where_uploads_go() {
        let own: &[&str] = &["--ingest-url", "https://ingest.example.com"];
        for (flags, upload) in [
            (&[][..], "https://app.example.com"),
            (own, "https://ingest.example.com"),
        ] {
            let args = ["api", "--public-url", "https://app.example.com"];
            let sign_in = ["--admin-password", "a long enough password"];
            let config = Config::parse_from(args.iter().chain(&sign_in).chain(flags));
            let app = router(AppState::new(config).unwrap());
            // No session, no database: it's the same answer for everyone.
            let asked = Request::get(ENDPOINTS_PATH).body(Body::empty()).unwrap();
            let answer = tower::ServiceExt::oneshot(app, asked).await.unwrap();
            assert_eq!(answer.status(), StatusCode::OK);
            let body = answer.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(
                serde_json::from_slice::<Endpoints>(&body).unwrap(),
                Endpoints {
                    upload: upload.into()
                }
            );
        }
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
