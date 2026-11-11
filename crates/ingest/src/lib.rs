//! Ground Station ingest: accepts event batches from gsd (`POST /v1/events`)
//! and stores them in ClickHouse for the tenant their token belongs to.
//!
//! Stateless: gsd keeps its own queue and retries until it gets a 2xx, so this
//! service can restart or scale at any time without losing events.

mod auth;
mod body;
mod error;
mod events;
mod ratelimit;
mod validate;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::extract::State;
use axum::routing::{get, post};
use groundstation_backend::ch::Clickhouse;
use groundstation_backend::config::{ClickhouseArgs, PostgresArgs, Service};
use groundstation_backend::pg::{self, Pool, tokens::Grant};
use groundstation_backend::{health, service};
use tokio::sync::Semaphore;

use crate::auth::Authenticator;
use crate::error::ApiError;
use crate::ratelimit::{Rate, RateLimiter};

/// Settings, from flags or environment variables. The defaults match
/// `docker-compose.yml`.
#[derive(clap::Parser, Debug)]
#[command(version, about = "Ground Station ingest service")]
pub struct Config {
    /// Address to listen on.
    #[arg(
        long,
        env = "GROUNDSTATION_INGEST_LISTEN",
        default_value = "127.0.0.1:4320"
    )]
    pub listen: SocketAddr,
    #[command(flatten)]
    pub clickhouse: ClickhouseArgs,
    #[command(flatten)]
    pub postgres: PostgresArgs,
    /// Requests per second one ingest token may sustain.
    #[arg(long, env = "GROUNDSTATION_INGEST_TOKEN_RATE", default_value_t = 20.0)]
    pub token_rate: f64,
    /// Requests one ingest token may send at once.
    #[arg(
        long,
        env = "GROUNDSTATION_INGEST_TOKEN_BURST",
        default_value_t = 200.0
    )]
    pub token_burst: f64,
    /// Requests per second all of a tenant's tokens together may sustain.
    #[arg(
        long,
        env = "GROUNDSTATION_INGEST_TENANT_RATE",
        default_value_t = 200.0
    )]
    pub tenant_rate: f64,
    #[arg(
        long,
        env = "GROUNDSTATION_INGEST_TENANT_BURST",
        default_value_t = 2000.0
    )]
    pub tenant_burst: f64,
    /// Token lookups in Postgres per second, across all clients. A token is
    /// looked up once a minute, so this is tokens in use divided by 60, with
    /// room; past it, requests with a token not seen in the last minute wait.
    #[arg(
        long,
        env = "GROUNDSTATION_INGEST_LOOKUP_RATE",
        default_value_t = 500.0
    )]
    pub lookup_rate: f64,
    /// Batches inflated, parsed and stored at once. Each can hold 64 MiB or
    /// more. Four times as many may be held, read but waiting; past that,
    /// requests are answered 429 to come back shortly.
    #[arg(long, env = "GROUNDSTATION_INGEST_DECODING", default_value_t = 16)]
    pub decoding: usize,
    /// How long a token's answer is reused, in seconds. For tests.
    #[arg(long, hide = true, default_value_t = 60)]
    pub token_cache_secs: u64,
    /// How long a client may take to send its batch, in seconds. gsd gives
    /// up after 30. For tests.
    #[arg(long, hide = true, default_value_t = 30)]
    pub read_timeout_secs: u64,
}

#[derive(Clone)]
pub struct AppState {
    clickhouse: Clickhouse,
    postgres: Pool,
    auth: Arc<Authenticator>,
    limits: Arc<Limits>,
    decoding: Arc<Semaphore>,
    /// Requests that hold a body, decoding or waiting to.
    held: Arc<Semaphore>,
    read_timeout: std::time::Duration,
}

impl AppState {
    pub fn new(config: &Config) -> anyhow::Result<Self> {
        let postgres = pg::connect(&config.postgres)?;
        Ok(Self {
            clickhouse: Clickhouse::connect(&config.clickhouse, Service::Ingest),
            auth: Arc::new(Authenticator::new(
                postgres.clone(),
                Rate {
                    per_second: config.lookup_rate,
                    burst: config.lookup_rate * 2.0,
                },
                std::time::Duration::from_secs(config.token_cache_secs),
            )),
            decoding: Arc::new(Semaphore::new(config.decoding.max(1))),
            held: Arc::new(Semaphore::new(config.decoding.max(1) * 4)),
            read_timeout: std::time::Duration::from_secs(config.read_timeout_secs),
            postgres,
            limits: Arc::new(Limits {
                token: RateLimiter::new(Rate {
                    per_second: config.token_rate,
                    burst: config.token_burst,
                }),
                tenant: RateLimiter::new(Rate {
                    per_second: config.tenant_rate,
                    burst: config.tenant_burst,
                }),
            }),
        })
    }
}

/// Per token and per tenant, so one runaway machine can't use up its whole
/// team's allowance.
struct Limits {
    token: RateLimiter,
    tenant: RateLimiter,
}

impl Limits {
    fn check(&self, grant: &Grant, now: Instant) -> Result<(), ApiError> {
        self.token
            .take(grant.token_id, now)
            .and_then(|()| self.tenant.take(grant.tenant_id, now))
            .map_err(ApiError::rate_limited)
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/events", post(events::ingest))
        .route("/healthz", get(healthz))
        .with_state(state)
}

async fn healthz(State(s): State<AppState>) -> impl axum::response::IntoResponse {
    health::report(&s.clickhouse, &s.postgres).await
}

pub async fn run(config: Config) -> anyhow::Result<()> {
    let state = AppState::new(&config)?;
    service::serve("groundstation-ingest", config.listen, router(state)).await?;
    Ok(())
}
