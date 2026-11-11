//! `/v1/insights` over HTTP.
//! `docker compose up -d --wait && cargo test -p groundstation-api -- --ignored`

mod common;

use std::collections::BTreeMap;

use axum::http::StatusCode;
use chrono::Utc;
use common::{app, get, json, person, send};
use groundstation_api::config::NewUsers;
use groundstation_backend::pg::insights::{self, Finding};
use groundstation_wire::insights::{Evidence, Severity};
use serde_json::json;

fn finding(scope: &str, severity: Severity) -> Finding {
    Finding {
        detector: "repeated_read",
        version: 1,
        scope: scope.into(),
        severity,
        title: format!("Read {scope} 3 times without changing it"),
        body: "body".into(),
        recommendation: "recommendation".into(),
        evidence: vec![Evidence {
            trajectory_id: "t1".into(),
            event_ids: vec![uuid::Uuid::now_v7()],
            note: "3 reads".into(),
        }],
        metrics: BTreeMap::from([("reads".to_string(), 3.0)]),
        evidence_received_at: Utc::now(),
    }
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn lists_and_triages_the_tenants_findings() {
    let (app, state) = app().await;
    let (tenant, session) = person(&state, NewUsers::Personal).await;
    let (_, other) = person(&state, NewUsers::Personal).await;
    for (scope, severity) in [("a.rs", Severity::Low), ("b.rs", Severity::High)] {
        insights::upsert(&state.pg, tenant, &finding(scope, severity), Utc::now())
            .await
            .unwrap();
    }

    let (status, _, health) = send(&app, get("/v1/health", Some(&session))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["features"], json!(["insights", "tokens"]));

    let (status, _, list) = send(&app, get("/v1/insights", Some(&session))).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list.as_array().unwrap().len(), 2);
    assert_eq!(list[0]["severity"], "high");
    assert_eq!(list[0]["evidence"][0]["trajectory_id"], "t1");
    let id = list[1]["id"].as_str().unwrap().to_string();

    let path = format!("/v1/insights/{id}/state");
    let (status, _, changed) = send(
        &app,
        json("POST", &path, &session, json!({"state": "dismissed"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(changed["state"], "dismissed");
    let (_, _, open) = send(&app, get("/v1/insights", Some(&session))).await;
    assert_eq!(open.as_array().unwrap().len(), 1);
    let (_, _, dismissed) = send(&app, get("/v1/insights?state=dismissed", Some(&session))).await;
    assert_eq!(dismissed[0]["id"], id.as_str());

    // Another tenant can't see or change it.
    let (status, ..) = send(&app, get(&format!("/v1/insights/{id}"), Some(&other))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, ..) = send(&app, json("POST", &path, &other, json!({"state": "open"}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, _, theirs) = send(&app, get("/v1/insights?state=all", Some(&other))).await;
    assert_eq!(theirs, json!([]));

    let (status, ..) = send(&app, get("/v1/insights?state=bogus", Some(&session))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
