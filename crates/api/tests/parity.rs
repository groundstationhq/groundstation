//! The hosted reads must answer exactly as gsd does for the same events.
//! Fixture trajectories go through gsd's store, and what gsd would upload goes
//! into ClickHouse; summaries, timelines, lists and tool stats must match.
//!
//! `docker compose up -d --wait && cargo test -p groundstation-api -- --ignored`

use chrono::{DateTime, Duration, Utc};
use groundstation_api::queries::{stats, trajectories};
use groundstation_backend::ch::{EventRow, TenantScope};
use groundstation_backend::testing::{self, Databases};
use groundstation_schema::{Agent, Event, EventKind, SCHEMA, attr};
use gsd::store::{NewEvent, Resolved, Store};
use serde_json::{Value, json};
use uuid::Uuid;

struct Fixture {
    t0: DateTime<Utc>,
    events: Vec<Event>,
}

impl Fixture {
    fn at(&mut self, traj: &str, secs: f64, kind: EventKind) -> &mut Event {
        let at = self.t0 + Duration::milliseconds((secs * 1000.0) as i64);
        let agent = Agent {
            name: "claude-code".into(),
            version: Some("2.1.0".into()),
        };
        self.events
            .push(Event::new(Uuid::now_v7(), traj, kind, at, agent));
        self.events.last_mut().unwrap()
    }
}

/// Two trajectories that exercise every summary rule.
fn fixture() -> Fixture {
    let mut f = Fixture {
        t0: Utc::now() - Duration::hours(1),
        events: Vec::new(),
    };
    let a = "parity-a";
    let e = f.at(a, 0.0, EventKind::AgentStarted);
    e.set(attr::VCS_REVISION, "aaa");
    e.set(attr::VCS_BRANCH, "main");
    e.set(attr::CWD, "web");
    let e = f.at(a, 1.0, EventKind::TurnUser);
    e.set(attr::PROMPT_TEXT, "\n  Fix the checkout tests\nplease");
    e.set(attr::VCS_REVISION, "aaa");
    for (span, start, end, category, failed) in [
        ("s1", 2.0, 5.0, "shell", true),
        ("s2", 6.0, 6.2, "file_read", false),
        ("s3", 6.3, 6.4, "file_read", false),
    ] {
        f.at(a, start, EventKind::ToolStarted)
            .set(attr::TOOL_CATEGORY, category);
        f.events.last_mut().unwrap().span_id = Some(span.into());
        let kind = if failed {
            EventKind::ToolFailed
        } else {
            EventKind::ToolCompleted
        };
        f.at(a, end, kind).set(attr::TOOL_CATEGORY, category);
        f.events.last_mut().unwrap().span_id = Some(span.into());
    }
    let e = f.at(a, 7.0, EventKind::ModelCompleted);
    e.set(attr::GEN_AI_INPUT_TOKENS, 10);
    e.set(attr::GEN_AI_OUTPUT_TOKENS, 20);
    e.set(attr::CACHE_READ_TOKENS, 18_000);
    e.set(attr::COST_USD, 0.0125);
    f.at(a, 8.0, EventKind::AgentNotification);
    f.at(a, 9.0, EventKind::TurnCompleted)
        .set(attr::VCS_REVISION, "bbb");
    f.at(a, 10.0, EventKind::AgentCompleted);
    f.at(a, 20.0, EventKind::AgentResumed);
    f.at(a, 21.0, EventKind::TurnUser)
        .set(attr::PROMPT_TEXT, "second prompt");
    // Read from a transcript: inherits the checkout in effect.
    f.at(a, 22.0, EventKind::ModelCompleted)
        .set(attr::GEN_AI_OUTPUT_TOKENS, 5);

    let b = "parity-b";
    f.at(b, 30.0, EventKind::AgentStarted);
    f.at(b, 31.0, EventKind::AgentCompleted);
    f
}

fn new(event: Event) -> NewEvent {
    NewEvent {
        event,
        raw: None,
        host: None,
    }
}

/// What gsd's queue holds for upload, as the uploader sends it
/// (`outbound::prepare`): paths relative to the repository, no host name.
/// These fixtures run in no repository, so their summaries don't change.
fn stored(store: &Store, ids: &[&str]) -> Vec<Event> {
    let privacy = gsd::privacy::Privacy::with_env(&gsd::config::Config::default(), []).unwrap();
    store
        .pending_upload(100_000)
        .unwrap()
        .into_iter()
        .filter(|p| ids.contains(&p.event.trajectory_id.as_str()))
        .map(|mut p| {
            gsd::outbound::prepare(&mut p.event, &p.place, &privacy);
            p.event
        })
        .collect()
}

async fn upload(
    db: &Databases,
    tenant: &groundstation_backend::pg::tenants::Tenant,
    events: &[Event],
) {
    let stamp = testing::stamp(tenant);
    let rows: Vec<EventRow> = events
        .iter()
        .map(|e| EventRow::new(e, SCHEMA, stamp).unwrap())
        .collect();
    db.ingest.insert_events(&rows).await.unwrap();
}

/// Loads the fixture both ways: gsd's store, and ClickHouse as uploaded,
/// including a redelivered batch and an updated model call.
async fn load() -> (Store, TenantScope, Fixture) {
    let db = testing::databases().await;
    let tenant = testing::tenant(&db.pg).await;
    let store = Store::open_in_memory().unwrap();
    let f = fixture();
    store
        .insert(f.events.iter().cloned().map(new).collect())
        .unwrap();
    let first = stored(&store, &["parity-a", "parity-b"]);
    upload(&db, &tenant, &first).await;
    upload(&db, &tenant, &first).await;

    // The transcript grew: the first model call now reports more tokens.
    let mut model = f
        .events
        .iter()
        .find(|e| e.kind == EventKind::ModelCompleted)
        .unwrap()
        .clone();
    model.set(attr::GEN_AI_OUTPUT_TOKENS, 42);
    assert_eq!(store.insert(vec![new(model.clone())]).unwrap(), 1);
    let updated = stored(&store, &["parity-a"])
        .into_iter()
        .find(|e| e.id == model.id)
        .unwrap();
    upload(&db, &tenant, &[updated]).await;
    (store, db.api.tenant(tenant.id), f)
}

/// Summaries as JSON, minus `host`, which never leaves the machine.
fn comparable<T: serde::Serialize>(value: &T) -> Value {
    let mut v = serde_json::to_value(value).unwrap();
    strip_host(&mut v);
    v
}

fn strip_host(v: &mut Value) {
    match v {
        Value::Object(map) => {
            map.remove("host");
            map.values_mut().for_each(strip_host);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_host),
        _ => {}
    }
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn summaries_and_timelines_match_gsd() {
    let (store, scope, _) = load().await;
    for id in ["parity-a", "parity-b"] {
        let (local, local_events) = store.trajectory(id).unwrap().unwrap();
        let (hosted, hosted_events) = trajectories::detail(&scope, id).await.unwrap().unwrap();
        assert_eq!(comparable(&hosted), comparable(&local), "summary of {id}");
        assert_eq!(hosted_events, local_events, "events of {id}");
    }
    let (summary, _) = trajectories::detail(&scope, "parity-a")
        .await
        .unwrap()
        .unwrap();
    // The rules the fixture exercises, spelled out.
    assert_eq!(summary.title.as_deref(), Some("Fix the checkout tests"));
    assert_eq!(
        (summary.status.as_str(), summary.ended_at),
        ("running", None)
    );
    assert_eq!(summary.revision.as_deref(), Some("bbb"));
    assert_eq!((summary.tool_calls, summary.tool_errors), (3, 1));
    assert_eq!((summary.output_tokens, summary.event_count), (47, 15));
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn lists_and_tool_stats_match_gsd() {
    let (store, scope, f) = load().await;
    let local = store.list_trajectories(50).unwrap();
    let hosted = trajectories::list(&scope, 50).await.unwrap();
    assert_eq!(comparable(&hosted), comparable(&local));
    assert_eq!(hosted[0].id, "parity-b");

    let since = f.t0 - Duration::days(1);
    let local = store
        .tool_latency(since.timestamp_nanos_opt().unwrap())
        .unwrap();
    let hosted = stats::tool_latency(&scope, since).await.unwrap();
    assert_eq!(comparable(&hosted), comparable(&local));
    assert_eq!(hosted[0].category, "file_read");
    // The window excludes calls that closed before it.
    let later = stats::tool_latency(&scope, f.t0 + Duration::seconds(6))
        .await
        .unwrap();
    assert_eq!(later.iter().map(|t| t.calls).sum::<u64>(), 2);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn prefixes_resolve_as_in_gsd() {
    let (store, scope, _) = load().await;
    for (query, expect) in [
        ("parity-b", "found"),
        ("parity", "ambiguous"),
        ("nope", "missing"),
    ] {
        let local = match store.resolve_trajectory(query).unwrap() {
            Resolved::Found(_) => "found",
            Resolved::Ambiguous(_) => "ambiguous",
            Resolved::NotFound => "missing",
        };
        let hosted = match trajectories::resolve(&scope, query).await.unwrap() {
            trajectories::Resolved::Found(_) => "found",
            trajectories::Resolved::Ambiguous(_) => "ambiguous",
            trajectories::Resolved::NotFound => "missing",
        };
        assert_eq!((local, hosted), (expect, expect), "{query}");
    }
}

/// Values no adapter writes but an SDK or a hand-made batch can: both hosts
/// must read them the same way, and neither may fail on them.
fn odd() -> Fixture {
    let mut f = Fixture {
        t0: Utc::now() - Duration::hours(1),
        events: Vec::new(),
    };
    // OpenCode-style ids: an underscore, and case that matters.
    let a = "Ses_Odd1";
    f.at(a, 0.0, EventKind::AgentStarted);
    let e = f.at(a, 1.0, EventKind::ModelCompleted);
    e.set(attr::GEN_AI_INPUT_TOKENS, 1.5);
    e.set(attr::GEN_AI_OUTPUT_TOKENS, -5);
    e.set(attr::CACHE_READ_TOKENS, "12");
    e.set(attr::CACHE_CREATION_TOKENS, true);
    // Ten calls at 10 cents add up to a dollar, not 0.9999999999999999.
    for i in 0..10 {
        f.at(a, 2.0 + f64::from(i) / 10.0, EventKind::ModelCompleted)
            .set(attr::COST_USD, 0.1);
    }
    f.at(a, 3.0, "Tool.Custom".parse().unwrap());
    for (i, duration) in [
        json!("NaN"),
        json!(null),
        json!(true),
        json!(1e300),
        json!(12.7),
        json!(40),
    ]
    .into_iter()
    .enumerate()
    {
        let e = f.at(a, 4.0 + i as f64, EventKind::ToolCompleted);
        e.attributes.insert(attr::DURATION_MS.into(), duration);
        e.set(attr::GEN_AI_TOOL_NAME, "Bash");
        e.set(attr::TOOL_CATEGORY, if i % 2 == 0 { "" } else { "shell" });
    }
    // A clock that was never set right, but after 1900.
    let old = "odd_old";
    let e = f.at(old, 0.0, EventKind::AgentCompleted);
    e.timestamp = "1950-06-01T00:00:00Z".parse().unwrap();
    // Two trajectories last updated at the same instant.
    for id in ["odd_tie_b", "odd_tie_a"] {
        f.at(id, 9.0, EventKind::AgentStarted);
    }
    f
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn odd_values_read_the_same_on_both_hosts() {
    let db = testing::databases().await;
    let tenant = testing::tenant(&db.pg).await;
    let store = Store::open_in_memory().unwrap();
    let f = odd();
    store
        .insert(f.events.iter().cloned().map(new).collect())
        .unwrap();
    let ids = ["Ses_Odd1", "odd_old", "odd_tie_a", "odd_tie_b"];
    upload(&db, &tenant, &stored(&store, &ids)).await;
    let scope = db.api.tenant(tenant.id);

    for id in ids {
        let (local, _) = store.trajectory(id).unwrap().unwrap();
        let (hosted, _) = trajectories::detail(&scope, id).await.unwrap().unwrap();
        assert_eq!(comparable(&hosted), comparable(&local), "summary of {id}");
    }
    let (summary, _) = store.trajectory("Ses_Odd1").unwrap().unwrap();
    assert_eq!(
        (summary.input_tokens, summary.output_tokens),
        (1, 0),
        "whole, never negative"
    );
    assert_eq!(
        (summary.cache_read_tokens, summary.cache_creation_tokens),
        (12, 0)
    );
    assert_eq!(summary.cost_usd, 1.0);
    assert_eq!(summary.tool_calls, 6, "Tool.Custom is not a tool kind");
    let (old, _) = store.trajectory("odd_old").unwrap().unwrap();
    assert_eq!(old.ended_at, Some("1950-06-01T00:00:00Z".parse().unwrap()));

    let local = store.list_trajectories(50).unwrap();
    let hosted = trajectories::list(&scope, 50).await.unwrap();
    assert_eq!(comparable(&hosted), comparable(&local));

    let since = f.t0 - Duration::days(1);
    let local = store
        .tool_latency(since.timestamp_nanos_opt().unwrap())
        .unwrap();
    let hosted = stats::tool_latency(&scope, since).await.unwrap();
    assert_eq!(comparable(&hosted), comparable(&local));
    let calls: u64 = local.iter().map(|t| t.calls).sum();
    assert_eq!(calls, 2, "only the numbers 12.7 and 40 are durations");

    for (query, expect) in [
        ("Ses_Odd", "found"),
        ("Ses_", "found"),
        ("ses_odd", "missing"),
        ("odd_tie", "ambiguous"),
    ] {
        let local = match store.resolve_trajectory(query).unwrap() {
            Resolved::Found(_) => "found",
            Resolved::Ambiguous(_) => "ambiguous",
            Resolved::NotFound => "missing",
        };
        let hosted = match trajectories::resolve(&scope, query).await.unwrap() {
            trajectories::Resolved::Found(_) => "found",
            trajectories::Resolved::Ambiguous(_) => "ambiguous",
            trajectories::Resolved::NotFound => "missing",
        };
        assert_eq!((local, hosted), (expect, expect), "{query}");
    }
}
