//! Findings the hosted backend draws from a tenant's events (`/v1/insights`,
//! hosted only: `insights` in `Health::features`).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Something specific that happened, with the events that show it and what
/// to change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Insight {
    pub id: Uuid,
    /// The rule that found it, e.g. `repeated_read`.
    pub detector: String,
    pub severity: Severity,
    pub title: String,
    pub body: String,
    pub recommendation: String,
    pub evidence: Vec<Evidence>,
    /// The numbers behind it, by name (e.g. `reads`, `failure_rate`).
    pub metrics: BTreeMap<String, f64>,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub state: InsightState,
}

/// Where to look: a trajectory, and the events in it that matter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub trajectory_id: String,
    pub event_ids: Vec<Uuid>,
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InsightState {
    Open,
    Dismissed,
    Resolved,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

impl InsightState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Dismissed => "dismissed",
            Self::Resolved => "resolved",
        }
    }
}

/// `POST /v1/insights/{id}/state`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateChange {
    pub state: InsightState,
}
