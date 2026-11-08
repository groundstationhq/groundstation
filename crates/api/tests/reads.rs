//! What one read can ask of the api and of ClickHouse.
//!
//! `docker compose up -d --wait && cargo test -p groundstation-api -- --ignored`

mod common;

use axum::http::StatusCode;
use chrono::{Duration, Utc};
use common::{app, get, person, send};
use groundstation_api::config::NewUsers;
use groundstation_backend::ch::EventRow;
use groundstation_backend::pg::tenants;
use groundstation_backend::testing;
use groundstation_schema::{EventKind, SCHEMA, attr};

async fn upload(
    state: &groundstation_api::AppState,
    tenant: uuid::Uuid,
    rows: impl Fn(&tenants::Tenant) -> Vec<EventRow>,
) {
    let tenant = tenants::get(&state.pg, tenant).await.unwrap().unwrap();
    let db = testing::databases().await;
    for chunk in rows(&tenant).chunks(5_000) {
        db.ingest.insert_events(chunk).await.unwrap();
    }
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn a_long_list_is_answered_whatever_the_ids() {
    let (app, state) = app().await;
    let (tenant, session) = person(&state, NewUsers::Personal).await;
    // Long ids, as an SDK may use: their list wouldn't fit in a request URL.
    upload(&state, tenant, |t| {
        (0..2_000)
            .map(|i| {
                let id = format!("{i:04}-{}", "x".repeat(60));
                let e = testing::event(
                    &id,
                    EventKind::AgentStarted,
                    Utc::now() - Duration::seconds(i),
                );
                EventRow::new(&e, SCHEMA, testing::stamp(t)).unwrap()
            })
            .collect()
    })
    .await;
    let (status, _, list) = send(&app, get("/v1/trajectories?limit=2000", Some(&session))).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list.as_array().unwrap().len(), 2_000);
    // Any limit gsd takes, clamped.
    let (status, ..) = send(
        &app,
        get("/v1/trajectories?limit=99999999999", Some(&session)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // An id longer than any stored one is simply not there.
    let long = format!("/v1/trajectories/{}", "y".repeat(300));
    let (status, _, body) = send(&app, get(&long, Some(&session))).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (StatusCode::NOT_FOUND, Some("trajectory not found"))
    );
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn a_large_timeline_streams_whole() {
    let (app, state) = app().await;
    let (tenant, session) = person(&state, NewUsers::Personal).await;
    let t0 = Utc::now() - Duration::hours(1);
    upload(&state, tenant, |t| {
        (0..3_000)
            .map(|i| {
                let mut e = testing::event(
                    "big",
                    EventKind::ToolCompleted,
                    t0 + Duration::milliseconds(i),
                );
                e.set(attr::TOOL_OUTPUT, "o".repeat(2_000));
                EventRow::new(&e, SCHEMA, testing::stamp(t)).unwrap()
            })
            .collect()
    })
    .await;
    let (status, _, detail) = send(&app, get("/v1/trajectories/big", Some(&session))).await;
    assert_eq!(status, StatusCode::OK);
    let events = detail["events"].as_array().unwrap();
    assert_eq!(
        (events.len(), detail["event_count"].as_u64()),
        (3_000, Some(3_000))
    );
    assert!(
        events
            .windows(2)
            .all(|w| w[0]["timestamp"].as_str() <= w[1]["timestamp"].as_str())
    );
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn health_answers_while_every_read_turn_is_taken() {
    let (app, state) = app().await;
    let (tenant, session) = person(&state, NewUsers::Personal).await;
    let t0 = Utc::now() - Duration::hours(1);
    upload(&state, tenant, |t| {
        (0..3_000)
            .map(|i| {
                let mut e = testing::event(
                    "big",
                    EventKind::ToolCompleted,
                    t0 + Duration::milliseconds(i),
                );
                e.set(attr::TOOL_OUTPUT, "o".repeat(2_000));
                EventRow::new(&e, SCHEMA, testing::stamp(t)).unwrap()
            })
            .collect()
    })
    .await;
    assert_eq!(
        send(&app, get("/v1/health", Some(&session))).await.0,
        StatusCode::OK
    );
    // Four timelines streaming to clients that read nothing hold every turn.
    let mut slow = Vec::new();
    for _ in 0..4 {
        let res =
            tower::ServiceExt::oneshot(app.clone(), get("/v1/trajectories/big", Some(&session)))
                .await
                .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        slow.push(res);
    }
    let started = std::time::Instant::now();
    let (status, _, health) = send(&app, get("/v1/health", Some(&session))).await;
    assert_eq!(status, StatusCode::OK, "{health}");
    assert_eq!(health["events"], 3_000);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    drop(slow);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn a_list_taken_while_trajectories_arrive_titles_every_row() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    let (app, state) = app().await;
    let (tenant, session) = person(&state, NewUsers::Personal).await;
    let tenant = tenants::get(&state.pg, tenant).await.unwrap().unwrap();
    // Machines starting trajectories, each with a prompt, as fast as they can:
    // the newest few change with every insert ClickHouse makes visible.
    let stop = Arc::new(AtomicBool::new(false));
    let writers: Vec<_> = (0..8)
        .map(|w| {
            let (stop, tenant) = (stop.clone(), tenant.clone());
            tokio::spawn(async move {
                let db = testing::databases().await;
                let mut i = 0;
                while !stop.load(Ordering::Relaxed) {
                    let mut e =
                        testing::event(&format!("w{w}-{i:05}"), EventKind::TurnUser, Utc::now());
                    e.set(attr::PROMPT_TEXT, format!("prompt {w}-{i}"));
                    let row = EventRow::new(&e, SCHEMA, testing::stamp(&tenant)).unwrap();
                    db.ingest.insert_events(&[row]).await.unwrap();
                    i += 1;
                }
            })
        })
        .collect();
    let (mut lists, mut rows, mut untitled) = (0, 0, Vec::new());
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(20) {
        let (status, _, list) = send(&app, get("/v1/trajectories?limit=3", Some(&session))).await;
        assert_eq!(status, StatusCode::OK, "{list}");
        lists += 1;
        for row in list.as_array().unwrap() {
            rows += 1;
            if row["title"].is_null() {
                untitled.push(row["id"].clone());
            }
        }
    }
    stop.store(true, Ordering::Relaxed);
    for w in writers {
        w.await.unwrap();
    }
    println!(
        "RACE {lists} lists, {rows} rows, {} untitled",
        untitled.len()
    );
    assert!(untitled.is_empty(), "untitled rows: {untitled:?}");
}
