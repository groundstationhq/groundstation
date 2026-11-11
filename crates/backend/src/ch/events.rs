//! One stored event: an [`Event`] as `gsd` sent it, plus who sent it and how
//! long to keep it.

use chrono::{DateTime, Duration, Utc};
use clickhouse::Row;
use groundstation_schema::{Agent, Event};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What the ingest service adds to every event of a request.
#[derive(Debug, Clone, Copy)]
pub struct Stamp {
    pub tenant_id: Uuid,
    pub token_id: Uuid,
    pub received_at: DateTime<Utc>,
    pub retain_until: DateTime<Utc>,
}

impl Stamp {
    /// For events received `now` from a tenant that keeps them for
    /// `retention_days`. `retain_until` is a ClickHouse `DateTime`, which ends
    /// in February 2106: a longer retention keeps events until then, rather
    /// than making every insert fail.
    pub fn new(tenant_id: Uuid, token_id: Uuid, retention_days: i32, now: DateTime<Utc>) -> Self {
        let last = DateTime::from_timestamp(i64::from(u32::MAX), 0).expect("in range");
        let retain_until = Duration::try_days(retention_days.into())
            .and_then(|keep| now.checked_add_signed(keep))
            .map_or(last, |until| until.min(last));
        Self {
            tenant_id,
            token_id,
            received_at: now,
            retain_until,
        }
    }
}

/// A row of `events`, in column order. The materialized columns are left out:
/// ClickHouse computes them from `attributes`.
#[derive(Debug, Clone, PartialEq, Row, Serialize, Deserialize)]
pub struct EventRow {
    #[serde(with = "clickhouse::serde::uuid")]
    pub tenant_id: Uuid,
    pub trajectory_id: String,
    #[serde(with = "clickhouse::serde::uuid")]
    pub event_id: Uuid,
    pub kind: String,
    #[serde(with = "clickhouse::serde::chrono::datetime64::nanos")]
    pub ts: DateTime<Utc>,
    pub span_id: Option<String>,
    pub parent_span_id: Option<String>,
    pub agent: String,
    pub agent_version: Option<String>,
    /// The attributes object, as JSON.
    pub attributes: String,
    pub schema: String,
    #[serde(with = "clickhouse::serde::uuid")]
    pub token_id: Uuid,
    #[serde(with = "clickhouse::serde::chrono::datetime64::millis")]
    pub received_at: DateTime<Utc>,
    #[serde(with = "clickhouse::serde::chrono::datetime")]
    pub retain_until: DateTime<Utc>,
}

impl EventRow {
    pub fn new(event: &Event, schema: &str, stamp: Stamp) -> serde_json::Result<Self> {
        Ok(Self {
            tenant_id: stamp.tenant_id,
            trajectory_id: event.trajectory_id.clone(),
            event_id: event.id,
            kind: event.kind.as_str().to_string(),
            ts: event.timestamp,
            span_id: event.span_id.clone(),
            parent_span_id: event.parent_span_id.clone(),
            agent: event.agent.name.clone(),
            agent_version: event.agent.version.clone(),
            attributes: serde_json::to_string(&event.attributes)?,
            schema: schema.to_string(),
            token_id: stamp.token_id,
            received_at: stamp.received_at,
            retain_until: stamp.retain_until,
        })
    }

    /// The event as `gsd` sent it.
    pub fn to_event(&self) -> serde_json::Result<Event> {
        Ok(Event {
            id: self.event_id,
            trajectory_id: self.trajectory_id.clone(),
            kind: self.kind.parse().unwrap_or_else(|never| match never {}),
            timestamp: self.ts,
            agent: Agent {
                name: self.agent.clone(),
                version: self.agent_version.clone(),
            },
            span_id: self.span_id.clone(),
            parent_span_id: self.parent_span_id.clone(),
            attributes: serde_json::from_str(&self.attributes)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use groundstation_schema::{EventKind, SCHEMA, attr};

    #[test]
    fn round_trips_an_event_including_unknown_kinds() {
        let mut event = Event::new(
            Uuid::now_v7(),
            "t1",
            "vendor.new_thing".parse::<EventKind>().unwrap(),
            Utc::now(),
            Agent {
                name: "codex".into(),
                version: Some("1.2".into()),
            },
        )
        .with_span("s1");
        event.set(attr::GEN_AI_INPUT_TOKENS, 12);
        let stamp = Stamp {
            tenant_id: Uuid::now_v7(),
            token_id: Uuid::now_v7(),
            received_at: Utc::now(),
            retain_until: Utc::now(),
        };
        let row = EventRow::new(&event, SCHEMA, stamp).unwrap();
        assert_eq!(row.kind, "vendor.new_thing");
        assert_eq!(row.to_event().unwrap(), event);
    }

    #[test]
    fn retention_ends_where_the_column_does() {
        let now = Utc::now();
        let stamp = |days| Stamp::new(Uuid::nil(), Uuid::nil(), days, now).retain_until;
        assert_eq!(stamp(30), now + Duration::days(30));
        let last = DateTime::from_timestamp(i64::from(u32::MAX), 0).unwrap();
        assert_eq!(stamp(40_000), last);
        assert_eq!(stamp(i32::MAX), last);
    }
}
