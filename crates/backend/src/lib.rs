//! Code shared by the backend services: ingest, api and the insights worker.
//!
//! - [`ch`]: ClickHouse. The `events` table, the trajectory index, their schema,
//!   and [`ch::TenantScope`], the only way to read events.
//! - [`pg`]: Postgres. Tenants, ingest tokens, and the migrations that create them.
//! - [`secret`]: generating and hashing tokens, which are stored only as hashes.
//! - [`config`]: connection settings, as flags or environment variables.
//! - [`service`]: logging, serving and shutting down; [`health`]: `/healthz`.

pub mod ch;
pub mod config;
mod error;
pub mod health;
pub mod pg;
pub mod secret;
pub mod service;
#[cfg(feature = "testing")]
pub mod testing;

pub use error::{Error, Result};
