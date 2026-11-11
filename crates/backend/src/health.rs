//! `GET /healthz` for every service: whether its databases answer. Internal:
//! the edge doesn't route it, and it says nothing about any tenant.

use std::future::Future;
use std::time::Duration;

use axum::Json;
use axum::http::StatusCode;
use serde::Serialize;

use crate::ch::Clickhouse;
use crate::pg::{self, Pool};

/// How long one check may take before it counts as down.
const TIMEOUT: Duration = Duration::from_secs(2);

/// 200 when every check passes, 503 otherwise.
#[derive(Debug, Serialize)]
pub struct Health {
    pub status: Status,
    pub version: &'static str,
    pub clickhouse: Check,
    pub postgres: Check,
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Down,
}

pub async fn report(ch: &Clickhouse, pg: &Pool) -> (StatusCode, Json<Health>) {
    let (clickhouse, postgres) = tokio::join!(
        check("clickhouse", ch.ping()),
        check("postgres", pg::ping(pg))
    );
    let healthy = clickhouse.status == Status::Ok && postgres.status == Status::Ok;
    let (code, status) = match healthy {
        true => (StatusCode::OK, Status::Ok),
        false => (StatusCode::SERVICE_UNAVAILABLE, Status::Down),
    };
    let body = Health {
        status,
        version: env!("CARGO_PKG_VERSION"),
        clickhouse,
        postgres,
    };
    (code, Json(body))
}

async fn check<T, E: std::fmt::Display>(
    name: &str,
    probe: impl Future<Output = Result<T, E>>,
) -> Check {
    let error = match tokio::time::timeout(TIMEOUT, probe).await {
        Ok(Ok(_)) => None,
        Ok(Err(e)) => Some(e.to_string()),
        Err(_) => Some(format!("no answer within {}s", TIMEOUT.as_secs())),
    };
    if let Some(e) = &error {
        tracing::warn!("{name} check failed: {e}");
    }
    let status = if error.is_none() {
        Status::Ok
    } else {
        Status::Down
    };
    Check { status, error }
}
