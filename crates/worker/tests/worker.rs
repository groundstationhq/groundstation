//! The worker against the databases from `docker-compose.yml`:
//! `docker compose up -d --wait && cargo test -p groundstation-worker -- --ignored`
//!
//! Each test passes over its own tenant only (`Worker::pass_tenant`), so tests
//! running side by side don't move each other's watermarks.

use chrono::{DateTime, Duration, Utc};
use clap::Parser;
use groundstation_backend::ch::{Clickhouse, EventRow, Stamp};
use groundstation_backend::config::Service;
use groundstation_backend::pg::insights;
use groundstation_backend::pg::tenants::Tenant;
use groundstation_backend::testing::{self, Databases};
use groundstation_schema::{Event, EventKind, SCHEMA, attr};
use groundstation_wire::insights::{Insight, InsightState, Severity};
use groundstation_worker::detectors::{Detector, Window};
use groundstation_worker::{Config, Worker};
use uuid::Uuid;

fn ago(secs: i64) -> DateTime<Utc> {
    Utc::now() - Duration::minutes(30) + Duration::seconds(secs)
}

fn tool(kind: EventKind, category: &str, secs: i64) -> Event {
    in_trajectory("w1", kind, category, secs)
}

fn in_trajectory(trajectory: &str, kind: EventKind, category: &str, secs: i64) -> Event {
    let mut e = testing::event(trajectory, kind, ago(secs));
    if !category.is_empty() {
        e.set(attr::TOOL_CATEGORY, category);
    }
    e
}

fn read(path: &str, secs: i64) -> Event {
    let mut e = tool(EventKind::ToolStarted, "file_read", secs);
    e.set(attr::FILE_PATH, path);
    e
}

async fn upload(db: &Databases, tenant: &Tenant, events: &[Event]) {
    upload_stamped(db, events, testing::stamp(tenant)).await;
}

async fn upload_stamped(db: &Databases, events: &[Event], stamp: Stamp) {
    let rows: Vec<EventRow> = events
        .iter()
        .map(|e| EventRow::new(e, SCHEMA, stamp).unwrap())
        .collect();
    db.ingest.insert_events(&rows).await.unwrap();
}

/// A worker that records exactly what it has seen, so a run with nothing new
/// changes nothing.
fn worker() -> Worker {
    Worker::new(&Config::parse_from(["worker", "--settle-secs", "0"])).unwrap()
}

async fn findings(db: &Databases, tenant: &Tenant) -> Vec<Insight> {
    insights::list(&db.pg, tenant.id, None, 500).await.unwrap()
}

/// Reads, a test loop and a failing tool category, all in trajectory w1.
fn fixture() -> Vec<Event> {
    let mut events = vec![tool(EventKind::TurnUser, "", 0)];
    events.extend((1..=3).map(|i| read("src/a.rs", i)));
    for i in 0..3 {
        let mut edit = tool(EventKind::ToolStarted, "file_write", 10 + i * 2);
        edit.set(attr::FILE_PATH, "src/b.rs");
        let mut test = tool(EventKind::ToolStarted, "shell", 11 + i * 2);
        test.set(attr::SHELL_COMMAND, "cargo test");
        events.extend([edit, test]);
    }
    events.extend(failing_calls("w1", 30));
    events
}

/// Twenty http calls, half of them failed.
fn failing_calls(trajectory: &str, from_secs: i64) -> Vec<Event> {
    (0..20)
        .map(|i| {
            let kind = if i % 2 == 0 {
                EventKind::ToolFailed
            } else {
                EventKind::ToolCompleted
            };
            let mut done = in_trajectory(trajectory, kind, "http", from_secs + i);
            done.set(attr::DURATION_MS, 100);
            done
        })
        .collect()
}

/// What makes each detector find exactly one thing, besides [`fixture`].
fn every_case() -> Vec<Event> {
    let mut events = fixture();
    // slow_tool: search calls went from 100ms to 1s since the week before.
    for i in 0..20 {
        let mut before = in_trajectory("s-old", EventKind::ToolCompleted, "search", 0);
        before.timestamp = Utc::now() - Duration::days(10) + Duration::seconds(i);
        before.set(attr::DURATION_MS, 100);
        let mut now = in_trajectory("s-new", EventKind::ToolCompleted, "search", 100 + i);
        now.set(attr::DURATION_MS, 1_000);
        events.extend([before, now]);
    }
    // cache_miss: ten trajectories read 80% from the cache, one 5%.
    let model_call = |trajectory: &str, input: u64, cached: u64| {
        let mut e = in_trajectory(trajectory, EventKind::ModelCompleted, "", 200);
        e.set(attr::GEN_AI_INPUT_TOKENS, input);
        e.set(attr::CACHE_READ_TOKENS, cached);
        e
    };
    events.extend((0..10).map(|i| model_call(&format!("c{i}"), 20_000, 80_000)));
    events.push(model_call("cold", 95_000, 5_000));
    // cost_outlier: twenty trajectories on m1 cost about a dollar, one nine.
    let costly = |trajectory: &str, usd: f64| {
        let mut e = in_trajectory(trajectory, EventKind::ModelCompleted, "", 300);
        e.set(attr::GEN_AI_RESPONSE_MODEL, "m1");
        e.set(attr::COST_USD, usd);
        e
    };
    events.extend((0..20).map(|i| costly(&format!("p{i}"), 1.0 + f64::from(i) / 100.0)));
    events.push(costly("pricey", 9.0));
    // permission_wait: a prompt nobody answered for eleven minutes.
    let mut asked = in_trajectory("pw", EventKind::AgentNotification, "", 400);
    asked.set(attr::NOTIFICATION_TYPE, "permission_prompt");
    events.extend([
        asked,
        in_trajectory("pw", EventKind::TurnUser, "", 400 + 660),
    ]);
    events
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn every_detector_finds_what_it_looks_for() {
    let db = testing::databases().await;
    let tenant = testing::tenant(&db.pg).await;
    upload(&db, &tenant, &every_case()).await;
    // As the worker's own ClickHouse user, which reads only what it needs.
    let clickhouse = Clickhouse::connect(&db.clickhouse_args, Service::Worker);
    let scope = clickhouse.tenant(tenant.id);
    let window = Window::ending(Utc::now(), 7);
    let expected = [
        (Detector::RepeatedRead, "w1\0src/a.rs", "reads", 3.0),
        (Detector::TestRerunLoop, "w1", "full_runs", 3.0),
        (Detector::FailureHotspot, "http", "failure_rate", 0.5),
        (Detector::SlowTool, "search", "p95_ms", 1_000.0),
        (Detector::CacheMiss, "cold", "cached_share", 0.05),
        (Detector::CostOutlier, "pricey", "cost_usd", 9.0),
        (Detector::PermissionWait, "pw", "wait_ms", 660_000.0),
    ];
    for (detector, scope_key, metric, value) in expected {
        let found = detector.run(&scope, window).await;
        let found = found.unwrap_or_else(|e| panic!("{}: {e}", detector.name()));
        let scopes: Vec<&str> = found.findings.iter().map(|f| f.scope.as_str()).collect();
        assert_eq!(scopes, [scope_key], "{}", detector.name());
        let f = &found.findings[0];
        assert_eq!(f.metrics[metric], value, "{}", detector.name());
        assert!(
            f.evidence.iter().all(|e| !e.event_ids.is_empty()),
            "{}: {:?}",
            detector.name(),
            f.evidence
        );
        let age = Utc::now() - f.evidence_received_at;
        assert!(age < Duration::minutes(5), "{}: {age}", detector.name());
        match detector {
            Detector::FailureHotspot => assert_eq!(f.evidence[0].note, "10 failed calls"),
            Detector::SlowTool => assert_eq!(f.evidence[0].note, "took 1.0s"),
            _ => {}
        }
    }
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn findings_left_out_over_the_cap_stay_as_they_were() {
    let db = testing::databases().await;
    let tenant = testing::tenant(&db.pg).await;
    let worker = worker();
    let reads = |trajectory: &str, n: i64| -> Vec<Event> {
        (0..n)
            .map(|i| {
                let mut e = in_trajectory(trajectory, EventKind::ToolStarted, "file_read", i);
                e.set(attr::FILE_PATH, "src/a.rs");
                e
            })
            .collect()
    };
    upload(&db, &tenant, &reads("lone", 3)).await;
    worker.pass_tenant(&tenant).await.unwrap();
    let lone = findings(&db, &tenant).await;
    assert_eq!(lone.len(), 1);

    // Two hundred worse ones push the first out of what a run keeps.
    let worse: Vec<Event> = (0..200).flat_map(|i| reads(&format!("t{i}"), 5)).collect();
    upload(&db, &tenant, &worse).await;
    worker.pass_tenant(&tenant).await.unwrap();
    let all = findings(&db, &tenant).await;
    assert_eq!(all.len(), 201);
    let first = all.iter().find(|f| f.id == lone[0].id).unwrap();
    assert_eq!(
        first.state,
        InsightState::Open,
        "not seen, but not gone either"
    );
    assert_eq!(first.last_seen_at, lone[0].last_seen_at);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn stores_findings_once_and_keeps_triage() {
    let db = testing::databases().await;
    let tenant = testing::tenant(&db.pg).await;
    let worker = worker();
    upload(&db, &tenant, &fixture()).await;

    worker.pass_tenant(&tenant).await.unwrap();
    let found = findings(&db, &tenant).await;
    let mut detectors: Vec<&str> = found.iter().map(|f| f.detector.as_str()).collect();
    detectors.sort();
    assert_eq!(
        detectors,
        ["repeated_read", "test_rerun_loop", "tool_failure_hotspot"]
    );
    let reads = found
        .iter()
        .find(|f| f.detector == "repeated_read")
        .unwrap();
    assert_eq!(reads.evidence[0].trajectory_id, "w1");
    assert_eq!(reads.evidence[0].event_ids.len(), 3);
    // Most severe first.
    assert_eq!(found[0].severity, Severity::Medium);

    // Nothing new: no detector runs, and nothing changes.
    let runs = last_runs(&db, &tenant).await;
    worker.pass_tenant(&tenant).await.unwrap();
    assert_eq!(findings(&db, &tenant).await, found);
    assert_eq!(last_runs(&db, &tenant).await, runs);

    let user = insert_user(&db).await;
    assert!(
        insights::set_state(&db.pg, tenant.id, reads.id, InsightState::Dismissed, user)
            .await
            .unwrap()
    );

    // Something new: every detector runs again, and finds the same three.
    upload(&db, &tenant, &[tool(EventKind::TurnCompleted, "", 60)]).await;
    worker.pass_tenant(&tenant).await.unwrap();
    assert_ne!(last_runs(&db, &tenant).await, runs);
    let again = findings(&db, &tenant).await;
    assert_eq!(ids(&again), ids(&found));
    // The same finding again stays dismissed...
    let reads_again = again.iter().find(|f| f.id == reads.id).unwrap();
    assert_eq!(reads_again.state, InsightState::Dismissed);

    // ...until it gets worse.
    upload(
        &db,
        &tenant,
        &(4..=6).map(|i| read("src/a.rs", 3 + i)).collect::<Vec<_>>(),
    )
    .await;
    worker.pass_tenant(&tenant).await.unwrap();
    let worse = insights::get(&db.pg, tenant.id, reads.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (worse.state, worse.severity),
        (InsightState::Open, Severity::Medium)
    );
    assert_eq!(worse.evidence[0].event_ids.len(), 6);
    assert_eq!(worse.first_seen_at, reads.first_seen_at);
    assert_eq!(ids(&findings(&db, &tenant).await), ids(&found));
}

fn ids(found: &[Insight]) -> Vec<Uuid> {
    let mut ids: Vec<Uuid> = found.iter().map(|f| f.id).collect();
    ids.sort();
    ids
}

/// When each detector last ran for the tenant.
async fn last_runs(db: &Databases, tenant: &Tenant) -> Vec<(String, DateTime<Utc>)> {
    db.pg
        .get()
        .await
        .unwrap()
        .query(
            "SELECT detector, last_run_at FROM insight_runs WHERE tenant_id = $1 ORDER BY detector",
            &[&tenant.id],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| (r.get(0), r.get(1)))
        .collect()
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn a_finding_closes_when_what_it_describes_stops() {
    let db = testing::databases().await;
    let tenant = testing::tenant(&db.pg).await;
    let worker = worker();
    let call = |kind, i| {
        let mut done = tool(kind, "http", i);
        done.set(attr::DURATION_MS, 100);
        done
    };

    // Half of 20 calls fail: a hotspot.
    upload(&db, &tenant, &failing_calls("w1", 0)).await;
    worker.pass_tenant(&tenant).await.unwrap();
    let open = insights::list(&db.pg, tenant.id, Some(InsightState::Open), 50)
        .await
        .unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].detector, "tool_failure_hotspot");

    // Forty calls that work bring the rate under one in five: no longer one.
    let working: Vec<Event> = (0..40)
        .map(|i| call(EventKind::ToolCompleted, 100 + i))
        .collect();
    upload(&db, &tenant, &working).await;
    worker.pass_tenant(&tenant).await.unwrap();
    let all = findings(&db, &tenant).await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].state, InsightState::Resolved);

    // It opens again if the failures come back.
    let failing_again: Vec<Event> = (0..60)
        .map(|i| call(EventKind::ToolFailed, 200 + i))
        .collect();
    upload(&db, &tenant, &failing_again).await;
    worker.pass_tenant(&tenant).await.unwrap();
    let again = insights::get(&db.pg, tenant.id, all[0].id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.state, InsightState::Open);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn tenants_keep_findings_of_their_own() {
    let db = testing::databases().await;
    let (a, b) = (testing::tenant(&db.pg).await, testing::tenant(&db.pg).await);
    let worker = worker();
    for tenant in [&a, &b] {
        upload(&db, tenant, &failing_calls("w1", 0)).await;
        worker.pass_tenant(tenant).await.unwrap();
    }
    // The same detector and scope ("http") in both: two findings.
    let (in_a, in_b) = (findings(&db, &a).await, findings(&db, &b).await);
    assert_eq!((in_a.len(), in_b.len()), (1, 1));
    assert_ne!(in_a[0].id, in_b[0].id);

    // A's calls start working; B's finding stays open.
    let working: Vec<Event> = (0..40)
        .map(|i| {
            let mut done = tool(EventKind::ToolCompleted, "http", 100 + i);
            done.set(attr::DURATION_MS, 100);
            done
        })
        .collect();
    upload(&db, &a, &working).await;
    worker.pass_tenant(&a).await.unwrap();
    assert_eq!(findings(&db, &a).await[0].state, InsightState::Resolved);
    assert_eq!(findings(&db, &b).await[0].state, InsightState::Open);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn a_batch_that_commits_after_a_later_one_is_looked_at() {
    // Ingest stamped batch A before batch B, but B committed first and a
    // run came between them.
    let db = testing::databases().await;
    let tenant = testing::tenant(&db.pg).await;
    let worker = Worker::new(&Config::parse_from(["worker"])).unwrap();
    upload(&db, &tenant, &[read("src/a.rs", 1)]).await;
    worker.pass_tenant(&tenant).await.unwrap();
    assert!(findings(&db, &tenant).await.is_empty());

    let stamped_earlier = Stamp::new(
        tenant.id,
        Uuid::now_v7(),
        tenant.retention_days,
        Utc::now() - Duration::seconds(30),
    );
    upload_stamped(
        &db,
        &[read("src/a.rs", 2), read("src/a.rs", 3)],
        stamped_earlier,
    )
    .await;
    worker.pass_tenant(&tenant).await.unwrap();
    let found = findings(&db, &tenant).await;
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].detector, "repeated_read");
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn events_stamped_ahead_of_the_clock_are_looked_at_on_arrival() {
    let db = testing::databases().await;
    let tenant = testing::tenant(&db.pg).await;
    let ahead: Vec<Event> = (0..3)
        .map(|i| {
            let mut e = read("src/a.rs", 0);
            e.timestamp = Utc::now() + Duration::minutes(90) + Duration::seconds(i);
            e
        })
        .collect();
    upload(&db, &tenant, &ahead).await;
    worker().pass_tenant(&tenant).await.unwrap();
    let found = findings(&db, &tenant).await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].detector, "repeated_read");
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn a_worker_allowed_one_connection_still_gets_through() {
    let db = testing::databases().await;
    let tenant = testing::tenant(&db.pg).await;
    upload(&db, &tenant, &fixture()).await;
    let config = Config::parse_from(["worker", "--postgres-max-connections", "1"]);
    let pass = Worker::new(&config)
        .unwrap()
        .pass_tenant(&tenant)
        .await
        .unwrap();
    assert_eq!((pass.stored, pass.failed), (3, 0));
}

async fn insert_user(db: &Databases) -> Uuid {
    let id = Uuid::now_v7();
    db.pg
        .get()
        .await
        .unwrap()
        .execute("INSERT INTO users (id) VALUES ($1)", &[&id])
        .await
        .unwrap();
    id
}
