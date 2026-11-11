//! Against the databases from `docker-compose.yml`:
//! `docker compose up -d --wait && cargo test -p groundstation-backend -- --ignored`

use std::collections::BTreeMap;

use chrono::{Duration, Utc};
use clap::Parser;
use groundstation_backend::ch::{Clickhouse, EventRow, TenantScope, deduplicated, schema};
use groundstation_backend::config::{ClickhouseAdminArgs, PostgresArgs, Service};
use groundstation_backend::pg::insights::{self, Finding};
use groundstation_backend::pg::{self, Pool, tenants, tokens};
use groundstation_backend::testing::{self, Databases, databases};
use groundstation_schema::{EventKind, SCHEMA, attr};
use groundstation_wire::insights::{InsightState, Severity};
use uuid::Uuid;

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn migrations_can_run_concurrently_and_repeatedly() {
    let db = databases().await;
    let (a, b) = tokio::join!(pg::migrate(&db.pg), pg::migrate(&db.pg));
    a.unwrap();
    b.unwrap();
    let (a, b) = tokio::join!(schema::migrate(&db.admin), schema::migrate(&db.admin));
    a.unwrap();
    b.unwrap();
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn migrations_create_a_fresh_database_concurrently() {
    // A name that only works quoted, as ClickHouse Cloud services often have.
    let db = databases().await;
    let name = format!("gs-test-{}", Uuid::now_v7().simple());
    db.admin
        .execute(&format!("CREATE DATABASE `{name}`"))
        .await
        .unwrap();
    let mut args = db.clickhouse_args.clone();
    args.clickhouse_database = name.clone();
    let admin = Clickhouse::connect_admin(&args, &Defaults::admin());
    let (a, b) = tokio::join!(schema::migrate(&admin), schema::migrate(&admin));
    let cleanup = async {
        for table in ["events", "traj_index"] {
            db.admin
                .execute(&format!(
                    "DROP ROW POLICY IF EXISTS gs_tenant_isolation ON `{name}`.{table}"
                ))
                .await
                .unwrap();
        }
        db.admin
            .execute(&format!("DROP DATABASE `{name}` SYNC"))
            .await
            .unwrap();
    };
    cleanup.await;
    a.unwrap();
    b.unwrap();
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn no_attribute_value_fails_an_insert() {
    // ClickHouse computes the hot columns while it inserts, for every request
    // in the same async-insert flush: one value it can't convert would fail
    // them all, across tenants, on every retry.
    let db = databases().await;
    let (good, odd) = (testing::tenant(&db.pg).await, testing::tenant(&db.pg).await);
    let fine = testing::event("t1", EventKind::AgentStarted, Utc::now());
    let fine = EventRow::new(&fine, SCHEMA, testing::stamp(&good)).unwrap();
    let odd_rows: Vec<EventRow> = [
        "\"NaN\"",
        "\"inf\"",
        "\"Infinity\"",
        "\"1e400\"",
        "null",
        "true",
        "1e300",
    ]
    .iter()
    .map(|value| {
        let mut event = testing::event("t1", EventKind::ToolCompleted, Utc::now());
        let value: serde_json::Value = serde_json::from_str(value).unwrap();
        event
            .attributes
            .insert(attr::DURATION_MS.into(), value.clone());
        event.attributes.insert(attr::COST_USD.into(), value);
        EventRow::new(&event, SCHEMA, testing::stamp(&odd)).unwrap()
    })
    .collect();
    let (a, b) = tokio::join!(
        db.ingest.insert_events(std::slice::from_ref(&fine)),
        db.ingest.insert_events(&odd_rows)
    );
    a.unwrap();
    b.unwrap();

    #[derive(clickhouse::Row, serde::Deserialize)]
    struct Hot {
        duration_ms: Option<i64>,
        cost_usd: f64,
    }
    let hot: Vec<Hot> = db
        .api
        .tenant(odd.id)
        .query("SELECT duration_ms, cost_usd FROM events WHERE tenant_id = {tenant:UUID}")
        .unwrap()
        .fetch_all()
        .await
        .unwrap();
    assert_eq!(hot.len(), odd_rows.len());
    for row in &hot {
        assert_eq!(row.duration_ms, None);
        assert!(row.cost_usd.is_finite());
    }
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn every_reader_needs_the_tenant_setting() {
    let db = databases().await;
    let tenant = testing::tenant(&db.pg).await;
    let event = testing::event("t1", EventKind::AgentStarted, Utc::now());
    let row = EventRow::new(&event, SCHEMA, testing::stamp(&tenant)).unwrap();
    db.ingest.insert_events(&[row]).await.unwrap();

    let a = &db.clickhouse_args;
    let as_user = |user: &str| {
        clickhouse::Client::default()
            .with_url(&a.clickhouse_url)
            .with_user(user)
            .with_password(&a.clickhouse_password)
            .with_database(&a.clickhouse_database)
    };
    for user in ["gs_api", "gs_worker"] {
        for table in ["events", "traj_index"] {
            let sql = format!("SELECT count() FROM {table}");
            for setting in [None, Some("not a uuid"), Some("")] {
                let mut query = as_user(user).query(&sql);
                if let Some(value) = setting {
                    query = query.with_option("SQL_tenant_id", value);
                }
                let seen: u64 = query.fetch_one().await.unwrap();
                assert_eq!(seen, 0, "{user} {table} {setting:?}");
            }
            let mine: u64 = as_user(user)
                .query(&sql)
                .with_option("SQL_tenant_id", tenant.id.to_string())
                .fetch_one()
                .await
                .unwrap();
            assert!(mine >= 1, "{user} {table}");
        }
    }
    for table in ["events", "traj_index"] {
        let read = as_user("gs_ingest")
            .query(&format!("SELECT count() FROM {table}"))
            .fetch_one::<u64>()
            .await;
        assert!(read.is_err(), "gs_ingest reads {table}");
    }
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn https_urls_speak_tls() {
    // The local ClickHouse answers plain HTTP only, so the handshake fails;
    // what matters is that the client tries, as ClickHouse Cloud needs.
    let db = databases().await;
    let mut args = db.clickhouse_args.clone();
    args.clickhouse_url = args.clickhouse_url.replacen("http://", "https://", 1);
    let err = Clickhouse::connect(&args, Service::Api)
        .ping()
        .await
        .unwrap_err();
    let mut chain = Vec::new();
    let mut cause: Option<&dyn std::error::Error> = Some(&err);
    while let Some(e) = cause {
        chain.push(e.to_string());
        cause = e.source();
    }
    assert!(!chain.iter().any(|e| e.contains("scheme")), "{chain:?}");
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn readers_see_only_the_tenant_they_name() {
    let db = databases().await;
    let (mine, theirs) = (testing::tenant(&db.pg).await, testing::tenant(&db.pg).await);
    let rows: Vec<EventRow> = [&mine, &theirs]
        .iter()
        .map(|t| {
            let event = testing::event("t1", EventKind::AgentStarted, Utc::now());
            EventRow::new(&event, SCHEMA, testing::stamp(t)).unwrap()
        })
        .collect();
    db.ingest.insert_events(&rows).await.unwrap();

    let count = |sql: &'static str| {
        let scope = db.api.tenant(mine.id);
        async move { scope.query(sql).unwrap().fetch_one::<u64>().await.unwrap() }
    };
    assert_eq!(
        count("SELECT count() FROM events WHERE tenant_id = {tenant:UUID}").await,
        1
    );
    // The row policy holds even if a query's own filter is wrong.
    assert_eq!(
        count("SELECT count() FROM events WHERE tenant_id = {tenant:UUID} OR 1").await,
        1
    );
    assert_eq!(
        count("SELECT count() FROM traj_index WHERE tenant_id = {tenant:UUID} OR 1").await,
        1
    );

    // A reader that names no tenant sees nothing.
    let a = &db.clickhouse_args;
    let raw = clickhouse::Client::default()
        .with_url(&a.clickhouse_url)
        .with_user("gs_api")
        .with_password(&a.clickhouse_password)
        .with_database(&a.clickhouse_database);
    let all: u64 = raw
        .query("SELECT count() FROM events")
        .fetch_one()
        .await
        .unwrap();
    assert_eq!(all, 0);

    // The ingest user can write but not read.
    let ingest = raw.with_user("gs_ingest");
    assert!(
        ingest
            .query("SELECT count() FROM events")
            .fetch_one::<u64>()
            .await
            .is_err()
    );
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn ingest_tokens_authenticate_until_revoked() {
    let db = databases().await;
    let tenant = testing::tenant(&db.pg).await;
    let (token, secret) = tokens::create(&db.pg, tenant.id, "laptop", None)
        .await
        .unwrap();
    assert!(token.last_used_at.is_none());

    let grant = tokens::authenticate(&db.pg, &secret)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((grant.token_id, grant.tenant_id), (token.id, tenant.id));
    assert_eq!((grant.retention_days, grant.suspended), (30, false));
    let listed = tokens::list(&db.pg, tenant.id).await.unwrap();
    assert!(listed[0].last_used_at.is_some());

    assert!(
        tokens::authenticate(&db.pg, "gsi_unknown")
            .await
            .unwrap()
            .is_none()
    );

    tenants::set_suspended(&db.pg, tenant.id, true)
        .await
        .unwrap();
    let grant = tokens::authenticate(&db.pg, &secret)
        .await
        .unwrap()
        .unwrap();
    assert!(grant.suspended);

    // Another tenant can't revoke it; its own tenant can.
    let other = testing::tenant(&db.pg).await;
    assert!(!tokens::revoke(&db.pg, other.id, token.id).await.unwrap());
    assert!(tokens::revoke(&db.pg, tenant.id, token.id).await.unwrap());
    assert!(
        tokens::authenticate(&db.pg, &secret)
            .await
            .unwrap()
            .is_none()
    );
}

/// The admin credentials the services default to.
#[derive(Parser)]
struct Defaults {
    #[command(flatten)]
    admin: ClickhouseAdminArgs,
}

impl Defaults {
    fn admin() -> ClickhouseAdminArgs {
        Self::parse_from(["test"]).admin
    }
}

/// A pool connected as one of the service roles from `docker/postgres/roles.sh`.
fn connect_as(db: &Databases, role: &str) -> Pool {
    let url = &db.postgres_args.postgres_url;
    let args = PostgresArgs {
        postgres_url: url.replacen("//groundstation:", &format!("//{role}:"), 1),
        postgres_max_connections: 2,
    };
    pg::connect(&args).unwrap()
}

async fn denied(pool: &Pool, sql: &str) -> bool {
    let conn = pool.get().await.unwrap();
    match conn.query(sql, &[]).await {
        Ok(_) => false,
        Err(e) => e.code() == Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE),
    }
}

fn finding(scope: &str, severity: Severity) -> Finding {
    Finding {
        detector: "repeated_read",
        version: 1,
        scope: scope.into(),
        severity,
        title: scope.into(),
        body: "b".into(),
        recommendation: "r".into(),
        evidence: Vec::new(),
        metrics: BTreeMap::new(),
        evidence_received_at: Utc::now(),
    }
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn service_roles_can_do_their_job_and_nothing_else() {
    let db = databases().await;
    let tenant = testing::tenant(&db.pg).await;
    let (_, secret) = tokens::create(&db.pg, tenant.id, "laptop", None)
        .await
        .unwrap();

    // A privilege granted by hand, or by an older release, goes at the next start.
    db.pg
        .get()
        .await
        .unwrap()
        .batch_execute("GRANT SELECT ON tenants, ingest_tokens TO gs_ingest")
        .await
        .unwrap();
    pg::migrate(&db.pg).await.unwrap();

    // Ingest resolves tokens, and reads no sessions, users, findings, or
    // anyone's names and labels.
    let ingest = connect_as(&db, pg::INGEST_ROLE);
    let grant = tokens::authenticate(&ingest, &secret).await.unwrap();
    assert_eq!(grant.unwrap().tenant_id, tenant.id);
    for sql in [
        "SELECT * FROM sessions",
        "SELECT * FROM users",
        "SELECT * FROM insights",
        "SELECT name FROM tenants",
        "SELECT label, created_by FROM ingest_tokens",
        "UPDATE ingest_tokens SET revoked_at = NULL",
        "UPDATE tenants SET suspended_at = NULL",
    ] {
        assert!(denied(&ingest, sql).await, "ingest may: {sql}");
    }

    // The worker reads tenants and writes findings, and never sees tokens.
    let worker = connect_as(&db, pg::WORKER_ROLE);
    assert!(!tenants::active(&worker).await.unwrap().is_empty());
    let f = finding("t1", Severity::Low);
    insights::upsert(&worker, tenant.id, &f, Utc::now())
        .await
        .unwrap();
    insights::record_run(&worker, tenant.id, f.detector, Utc::now())
        .await
        .unwrap();
    insights::sweep(&worker, 7).await.unwrap();
    for sql in [
        "SELECT * FROM ingest_tokens",
        "SELECT * FROM sessions",
        "UPDATE tenants SET suspended_at = NULL",
    ] {
        assert!(denied(&worker, sql).await, "the worker may: {sql}");
    }
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn findings_with_odd_metrics_stay_readable() {
    // Odd client values (a cost of "inf", sums that overflow) can give a
    // detector a non-finite number, which JSON can't hold.
    let db = databases().await;
    let tenant = testing::tenant(&db.pg).await;
    let mut f = finding("odd", Severity::High);
    f.metrics = BTreeMap::from([
        ("cost_usd".into(), f64::INFINITY),
        ("median_cost_usd".into(), f64::NAN),
        ("p95_cost_usd".into(), 1.5),
    ]);
    insights::upsert(&db.pg, tenant.id, &f, Utc::now())
        .await
        .unwrap();
    // A row stored before upsert left such numbers out holds a null.
    db.pg
        .get()
        .await
        .unwrap()
        .execute(
            "UPDATE insights SET metrics = metrics || '{\"older\": null}' WHERE tenant_id = $1",
            &[&tenant.id],
        )
        .await
        .unwrap();

    let listed = insights::list(&db.pg, tenant.id, None, 50).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].metrics,
        BTreeMap::from([("p95_cost_usd".into(), 1.5)])
    );
    let one = insights::get(&db.pg, tenant.id, listed[0].id)
        .await
        .unwrap();
    assert_eq!(one.unwrap().metrics, listed[0].metrics);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn findings_can_carry_any_text_an_event_had() {
    let db = databases().await;
    let tenant = testing::tenant(&db.pg).await;
    let mut f = finding("nul", Severity::Low);
    f.title = "Read src/a\0b.rs 3 times".into();
    f.evidence = vec![groundstation_wire::insights::Evidence {
        trajectory_id: "t\0".into(),
        event_ids: Vec::new(),
        note: "src/a\0b.rs".into(),
    }];
    insights::upsert(&db.pg, tenant.id, &f, Utc::now())
        .await
        .unwrap();
    let stored = &insights::list(&db.pg, tenant.id, None, 50).await.unwrap()[0];
    assert_eq!(stored.title, "Read src/a\u{fffd}b.rs 3 times");
    assert_eq!(stored.evidence[0].trajectory_id, "t\u{fffd}");
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn findings_close_when_unseen_and_go_when_their_events_do() {
    let db = databases().await;
    let tenant = testing::tenant(&db.pg).await;
    // Two findings from one run; the next run finds only the first again.
    let first_run = Utc::now() - Duration::minutes(10);
    let (stays, stops) = (
        finding("stays", Severity::Low),
        finding("stops", Severity::Low),
    );
    for f in [&stays, &stops] {
        insights::upsert(&db.pg, tenant.id, f, first_run)
            .await
            .unwrap();
    }
    let second_run = Utc::now();
    insights::upsert(&db.pg, tenant.id, &stays, second_run)
        .await
        .unwrap();
    let closed = insights::resolve_unseen(&db.pg, tenant.id, stays.detector, second_run)
        .await
        .unwrap();
    assert_eq!(closed, 1);
    let open = insights::list(&db.pg, tenant.id, Some(InsightState::Open), 50)
        .await
        .unwrap();
    assert_eq!(open.len(), 1);
    let resolved = insights::list(&db.pg, tenant.id, Some(InsightState::Resolved), 50)
        .await
        .unwrap();
    assert_eq!(resolved.len(), 1);

    // Closed by the worker, it opens again when it comes back, no worse than before.
    insights::upsert(&db.pg, tenant.id, &stops, Utc::now())
        .await
        .unwrap();
    let reopened = insights::get(&db.pg, tenant.id, resolved[0].id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reopened.state, InsightState::Open);

    // The sweep closes what no run has seen for the window...
    let stale = finding("stale", Severity::Low);
    insights::upsert(&db.pg, tenant.id, &stale, Utc::now() - Duration::days(8))
        .await
        .unwrap();
    // ...and deletes what describes events the tenant no longer keeps (30
    // days here), however recently a run saw it: a detector looks back a
    // week, so it can see a finding days after its newest event arrived.
    let mut expired = finding("expired", Severity::High);
    expired.evidence_received_at = Utc::now() - Duration::days(31);
    insights::upsert(&db.pg, tenant.id, &expired, Utc::now() - Duration::days(25))
        .await
        .unwrap();
    let mut kept = finding("kept", Severity::High);
    kept.evidence_received_at = Utc::now() - Duration::days(29);
    insights::upsert(&db.pg, tenant.id, &kept, Utc::now() - Duration::days(25))
        .await
        .unwrap();
    // A row from before findings recorded their events' age goes a retention
    // period after it was last seen.
    let older = finding("older", Severity::Low);
    insights::upsert(&db.pg, tenant.id, &older, Utc::now() - Duration::days(31))
        .await
        .unwrap();
    db.pg
        .get()
        .await
        .unwrap()
        .execute(
            "UPDATE insights SET evidence_received_at = NULL WHERE tenant_id = $1 AND title = 'older'",
            &[&tenant.id],
        )
        .await
        .unwrap();
    insights::sweep(&db.pg, 7).await.unwrap();
    let all = insights::list(&db.pg, tenant.id, None, 50).await.unwrap();
    let mut titles: Vec<&str> = all.iter().map(|f| f.title.as_str()).collect();
    titles.sort();
    assert_eq!(titles, ["kept", "stale", "stays", "stops"]);
    let open = insights::list(&db.pg, tenant.id, Some(InsightState::Open), 50)
        .await
        .unwrap();
    assert_eq!(open.len(), 2, "the stale and kept findings are closed");
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn an_older_run_never_overwrites_a_newer_one() {
    // Two workers: the one that started first writes last.
    let db = databases().await;
    let tenant = testing::tenant(&db.pg).await;
    let (older_run, newer_run) = (Utc::now() - Duration::minutes(5), Utc::now());
    let mut newer = finding("f", Severity::Medium);
    newer.body = "newer".into();
    insights::upsert(&db.pg, tenant.id, &newer, newer_run)
        .await
        .unwrap();
    let mut older = finding("f", Severity::High);
    older.body = "older".into();
    insights::upsert(&db.pg, tenant.id, &older, older_run)
        .await
        .unwrap();
    let stored = &insights::list(&db.pg, tenant.id, None, 50).await.unwrap()[0];
    assert_eq!(
        (stored.body.as_str(), stored.severity),
        ("newer", Severity::Medium)
    );
    assert_eq!(
        stored.last_seen_at.timestamp_micros(),
        newer_run.timestamp_micros()
    );
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn a_run_stores_all_its_findings_at_once() {
    let db = databases().await;
    let tenant = testing::tenant(&db.pg).await;
    let mut run: Vec<Finding> = (0..300)
        .map(|i| finding(&format!("f{i}"), Severity::Low))
        .collect();
    // The same fingerprint twice in one run: the more severe is kept.
    run.push(finding("f7", Severity::High));
    insights::store(&db.pg, tenant.id, &run, Utc::now())
        .await
        .unwrap();
    let stored = insights::list(&db.pg, tenant.id, None, 500).await.unwrap();
    assert_eq!(stored.len(), 300);
    assert_eq!(stored[0].severity, Severity::High);
    insights::store(&db.pg, tenant.id, &[], Utc::now())
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn postgres_errors_say_what_went_wrong() {
    let db = databases().await;
    let err = db
        .pg
        .get()
        .await
        .unwrap()
        .execute(
            "INSERT INTO tenants (id, name, retention_days) VALUES ($1, 'x', 0)",
            &[&Uuid::now_v7()],
        )
        .await
        .unwrap_err();
    let shown = groundstation_backend::Error::from(err).to_string();
    assert!(shown.contains("violates check constraint"), "{shown}");
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn a_tenants_reads_are_bounded() {
    let db = databases().await;
    let tenant = testing::tenant(&db.pg).await;
    let setting_of = |scope: TenantScope, name: &'static str| async move {
        scope
            .query(&format!(
                "SELECT toString(getSetting('{name}')) WHERE {{tenant:UUID}} = {{tenant:UUID}}"
            ))
            .unwrap()
            .fetch_one::<String>()
            .await
            .unwrap()
    };
    let setting = |name| setting_of(db.api.tenant(tenant.id), name);
    assert_eq!(setting("max_execution_time").await, "30");
    assert_eq!(setting("max_memory_usage").await, (2u64 << 30).to_string());
    assert_eq!(
        setting("max_bytes_before_external_sort").await,
        (1u64 << 30).to_string()
    );
    // A read at its reader's pace gets more time, and no more memory.
    let unhurried = |name| setting_of(db.api.tenant(tenant.id).unhurried(), name);
    assert_eq!(unhurried("max_execution_time").await, "600");
    assert_eq!(
        unhurried("max_memory_usage").await,
        (2u64 << 30).to_string()
    );
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn a_read_sees_only_the_newest_copy_of_an_event() {
    let db = databases().await;
    let tenant = testing::tenant(&db.pg).await;
    let now = Utc::now();
    // A model call uploaded three times: last month, then twice today, the
    // last time with more tokens. The copies are in two partitions.
    let mut call = testing::event("t1", EventKind::ModelCompleted, now);
    for (received_at, tokens) in [
        (now - Duration::days(40), 1),
        (now - Duration::seconds(2), 1),
        (now, 4242),
    ] {
        call.set(attr::GEN_AI_OUTPUT_TOKENS, tokens);
        let mut stamp = testing::stamp(&tenant);
        stamp.received_at = received_at;
        let row = EventRow::new(&call, SCHEMA, stamp).unwrap();
        // One insert each, so the copies start out in separate parts.
        db.ingest.insert_events(&[row]).await.unwrap();
    }
    let read = |filter: &'static str| {
        let scope = db.api.tenant(tenant.id);
        let sql = format!(
            "SELECT output_tokens FROM ({})",
            deduplicated("output_tokens", filter)
        );
        async move { scope.query(&sql).unwrap().fetch_all::<u64>().await.unwrap() }
    };
    assert_eq!(read("1").await, [4242]);
    // A filter never brings back a copy that a newer one replaced.
    assert_eq!(read("output_tokens = 1").await, [0u64; 0]);
}
