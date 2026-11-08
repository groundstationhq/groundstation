//! Checks on each event, and turning it into a stored row. A problem with one
//! event refuses only that event (gsd drops it after repeated refusals), so
//! nothing here may depend on the backend's own state.

use chrono::{DateTime, Duration, TimeZone, Utc};
use groundstation_backend::ch::{EventRow, Stamp};
use groundstation_schema::Event;
use serde::Deserialize;
use serde_json::value::RawValue;
use uuid::Uuid;

pub const MAX_TRAJECTORY_ID: usize = 256;
pub const MAX_ATTRIBUTES: usize = groundstation_wire::MAX_EVENT_ATTRIBUTES;
pub const MAX_FUTURE: Duration = groundstation_wire::MAX_FUTURE;
/// The first year the `ts` column (`DateTime64(9)`) holds. An earlier
/// timestamp is a zero value some client never set, and it must refuse that
/// one event: left in, the insert fails and the whole batch looks like a
/// storage outage that never clears.
pub const MIN_YEAR: i32 = 1900;

#[derive(Debug, PartialEq)]
pub enum Invalid {
    /// Without a usable id the event can't be refused by id.
    NoId,
    Event {
        id: Uuid,
        error: &'static str,
    },
}

pub fn to_row(
    raw: &RawValue,
    schema: &str,
    stamp: Stamp,
    now: DateTime<Utc>,
) -> Result<EventRow, Invalid> {
    let event: Event =
        serde_json::from_str(raw.get()).map_err(|_| refuse(raw, "not a valid event"))?;
    let refused = |error| Invalid::Event {
        id: event.id,
        error,
    };
    if event.trajectory_id.is_empty() {
        return Err(refused("trajectory_id is empty"));
    }
    if event.trajectory_id.len() > MAX_TRAJECTORY_ID {
        return Err(refused("trajectory_id is longer than 256 bytes"));
    }
    if event.timestamp > now + MAX_FUTURE {
        return Err(refused("timestamp is more than 24h in the future"));
    }
    if event.timestamp < Utc.with_ymd_and_hms(MIN_YEAR, 1, 1, 0, 0, 0).unwrap() {
        return Err(refused("timestamp is before 1900"));
    }
    let row =
        EventRow::new(&event, schema, stamp).map_err(|_| refused("attributes can't be encoded"))?;
    if row.attributes.len() > MAX_ATTRIBUTES {
        return Err(refused("attributes are larger than 1 MiB"));
    }
    Ok(row)
}

/// Refuses an event that didn't parse, by its id if it has a usable one.
/// Never echoes the input: parse errors can quote attribute values.
fn refuse(raw: &RawValue, error: &'static str) -> Invalid {
    #[derive(Deserialize)]
    struct Id {
        id: Uuid,
    }
    match serde_json::from_str::<Id>(raw.get()) {
        Ok(Id { id }) => Invalid::Event { id, error },
        Err(_) => Invalid::NoId,
    }
}

#[cfg(test)]
mod tests {
    use groundstation_schema::{Agent, EventKind, SCHEMA};
    use serde_json::json;

    use super::*;

    fn stamp() -> Stamp {
        Stamp {
            tenant_id: Uuid::nil(),
            token_id: Uuid::nil(),
            received_at: Utc::now(),
            retain_until: Utc::now(),
        }
    }

    fn check(value: serde_json::Value) -> Result<EventRow, Invalid> {
        let raw = RawValue::from_string(value.to_string()).unwrap();
        to_row(&raw, SCHEMA, stamp(), Utc::now())
    }

    fn event(trajectory: &str, at: DateTime<Utc>) -> serde_json::Value {
        serde_json::to_value(Event::new(
            Uuid::now_v7(),
            trajectory,
            "vendor.future_kind".parse::<EventKind>().unwrap(),
            at,
            Agent::named("x"),
        ))
        .unwrap()
    }

    fn error(result: Result<EventRow, Invalid>) -> &'static str {
        match result {
            Err(Invalid::Event { error, .. }) => error,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn accepts_unknown_kinds_and_old_timestamps() {
        let row = check(event("t", Utc::now() - Duration::days(400))).unwrap();
        assert_eq!(row.kind, "vendor.future_kind");
    }

    #[test]
    fn refuses_what_would_break_storage_or_queries() {
        assert_eq!(
            error(check(event("", Utc::now()))),
            "trajectory_id is empty"
        );
        assert_eq!(
            error(check(event(&"x".repeat(257), Utc::now()))),
            "trajectory_id is longer than 256 bytes"
        );
        assert_eq!(
            error(check(event("t", Utc::now() + Duration::hours(25)))),
            "timestamp is more than 24h in the future"
        );
        let zero = "0001-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        for unset in [
            zero,
            Utc.with_ymd_and_hms(1899, 12, 31, 23, 59, 59).unwrap(),
        ] {
            assert_eq!(error(check(event("t", unset))), "timestamp is before 1900");
        }
        assert!(
            check(event(
                "t",
                Utc.with_ymd_and_hms(1970, 1, 1, 0, 0, 0).unwrap()
            ))
            .is_ok()
        );
        let mut big = event("t", Utc::now());
        big["attributes"] = json!({ "gs.tool.output.body": "x".repeat(MAX_ATTRIBUTES) });
        assert_eq!(error(check(big)), "attributes are larger than 1 MiB");
    }

    #[test]
    fn a_malformed_event_is_refused_by_id_without_echoing_it() {
        let id = Uuid::now_v7();
        let bad = json!({ "id": id, "trajectory_id": "t", "kind": "agent.started",
                          "timestamp": "secret prompt text", "agent": { "name": "x" } });
        assert_eq!(
            check(bad),
            Err(Invalid::Event {
                id,
                error: "not a valid event"
            })
        );
        assert_eq!(check(json!({ "id": "nope" })), Err(Invalid::NoId));
    }
}
