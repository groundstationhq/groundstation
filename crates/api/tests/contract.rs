//! The `/v1` read contract, host against host: the same events posted to gsd
//! and uploaded to the hosted api must read back as the same bodies, errors
//! included. `host` differs, since hostnames never leave the machine; so do
//! `cwd`, `repository` and the paths in events once they come from a
//! repository, since those leave relative to it. These events come from none.
//!
//! `docker compose up -d --wait && cargo test -p groundstation-api -- --ignored`

mod common;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use chrono::{Duration, Utc};
use common::{app, get, person, send};
use groundstation_api::config::NewUsers;
use groundstation_backend::ch::EventRow;
use groundstation_backend::pg::tenants;
use groundstation_backend::testing;
use groundstation_schema::{Event, EventKind, SCHEMA, attr};
use gsd::config::Config;
use gsd::ingest::Ingestor;
use gsd::privacy::Privacy;
use gsd::server::{AppState, router};
use gsd::store::Store;
use gsd::uploader::UploadState;
use serde_json::{Value, json};

fn gsd() -> Router {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let ingestor = Arc::new(Ingestor::new(
        store,
        Privacy::with_env(&Config::default(), []).unwrap(),
    ));
    let (shutdown, _) = tokio::sync::watch::channel(false);
    router(AppState {
        ingestor,
        config: Arc::new(Config::default()),
        upload: UploadState::default(),
        shutdown,
    })
}

async fn local(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:4318");
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    let body = body.map_or(Body::empty(), |b| Body::from(b.to_string()));
    let (status, _, value) = send(app, req.body(body).unwrap()).await;
    (status, value)
}

fn events() -> Vec<Event> {
    let t0 = Utc::now() - Duration::hours(2);
    let at = |s| t0 + Duration::seconds(s);
    let mut prompt = testing::event("contract-a", EventKind::TurnUser, at(0));
    prompt.set(attr::PROMPT_TEXT, "Make the build green");
    let mut open = testing::event("contract-a", EventKind::ToolStarted, at(1)).with_span("s1");
    open.set(attr::TOOL_CATEGORY, "shell");
    let mut close = testing::event("contract-a", EventKind::ToolCompleted, at(4)).with_span("s1");
    close.set(attr::TOOL_CATEGORY, "shell");
    let mut model = testing::event("contract-a", EventKind::ModelCompleted, at(5));
    model.set(attr::GEN_AI_OUTPUT_TOKENS, 120);
    model.set(attr::COST_USD, 0.02);
    vec![
        prompt,
        open,
        close,
        model,
        testing::event("contract-a", EventKind::AgentCompleted, at(6)),
        testing::event("contract-b", EventKind::AgentStarted, at(10)),
    ]
}

fn without_host(mut v: Value) -> Value {
    match &mut v {
        Value::Object(map) => {
            map.remove("host");
            for value in map.values_mut() {
                *value = without_host(value.take());
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                *item = without_host(item.take());
            }
        }
        _ => {}
    }
    v
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn both_hosts_answer_the_same() {
    let gsd = gsd();
    let batch = json!({ "schema": SCHEMA, "events": events() });
    assert_eq!(
        local(&gsd, "POST", "/v1/events", Some(batch)).await.0,
        StatusCode::OK
    );

    // What gsd uploads is what it stored, with its durations filled in.
    let mut stored = Vec::new();
    for id in ["contract-a", "contract-b"] {
        let (_, detail) = local(&gsd, "GET", &format!("/v1/trajectories/{id}"), None).await;
        stored.extend(serde_json::from_value::<Vec<Event>>(detail["events"].clone()).unwrap());
    }
    let (api, state) = app().await;
    let (tenant, session) = person(&state, NewUsers::Personal).await;
    let tenant = tenants::get(&state.pg, tenant).await.unwrap().unwrap();
    let rows: Vec<EventRow> = stored
        .iter()
        .map(|e| EventRow::new(e, SCHEMA, testing::stamp(&tenant)).unwrap())
        .collect();
    testing::databases()
        .await
        .ingest
        .insert_events(&rows)
        .await
        .unwrap();

    for uri in [
        "/v1/trajectories",
        "/v1/trajectories?limit=1",
        "/v1/trajectories/contract-a",
        "/v1/trajectories/contract-b",
        "/v1/trajectories/contract", // ambiguous prefix: 409
        "/v1/trajectories/nope",     // 404
        "/v1/stats/tools",
        "/v1/stats/tools?days=9999",
    ] {
        let (local_status, local_body) = local(&gsd, "GET", uri, None).await;
        let (hosted_status, _, hosted_body) = send(&api, get(uri, Some(&session))).await;
        assert_eq!(hosted_status, local_status, "{uri}");
        let (mut local_body, mut hosted_body) =
            (without_host(local_body), without_host(hosted_body));
        // `since` is computed from the clock at each request.
        for body in [&mut local_body, &mut hosted_body] {
            if let Some(map) = body.as_object_mut() {
                map.remove("since");
            }
        }
        assert_eq!(hosted_body, local_body, "{uri}");
    }

    let (_, local_health) = local(&gsd, "GET", "/v1/health", None).await;
    let (_, _, hosted_health) = send(&api, get("/v1/health", Some(&session))).await;
    for field in ["status", "schema"] {
        assert_eq!(hosted_health[field], local_health[field], "health.{field}");
    }
    // Counting is a query on the hosted backend, so its health leaves it out.
    for field in ["trajectories", "events"] {
        assert!(local_health[field].is_u64(), "health.{field}");
        assert!(hosted_health.get(field).is_none(), "health.{field}");
    }
}
