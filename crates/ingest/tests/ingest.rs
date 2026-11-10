//! `POST /v1/events` against the databases from `docker-compose.yml`:
//! `docker compose up -d --wait && cargo test -p groundstation-ingest -- --ignored`

use std::io::Write;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use clap::Parser;
use flate2::Compression;
use flate2::write::GzEncoder;
use futures_util::StreamExt;
use groundstation_backend::ch::Clickhouse;
use groundstation_backend::config::Service;
use groundstation_backend::pg::{tenants, tokens};
use groundstation_backend::secret;
use groundstation_backend::testing::{self, Databases};
use groundstation_ingest::{AppState, Config, router};
use groundstation_schema::{Event, EventKind, SCHEMA, attr};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

struct Harness {
    app: Router,
    db: Databases,
    tenant: Uuid,
    token: String,
}

async fn harness(flags: &[&str]) -> Harness {
    let db = testing::databases().await;
    let tenant = testing::tenant(&db.pg).await;
    let (_, token) = tokens::create(&db.pg, tenant.id, "test", None)
        .await
        .unwrap();
    let args = ["groundstation-ingest"].iter().chain(flags);
    let app = router(AppState::new(&Config::parse_from(args)).unwrap());
    Harness {
        app,
        db,
        tenant: tenant.id,
        token,
    }
}

/// A token shaped like the ones this backend issues, which it never issued.
fn made_up() -> String {
    secret::generate(secret::INGEST_TOKEN_PREFIX)
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

fn batch(events: &[Event]) -> Vec<u8> {
    gzip(
        json!({ "schema": SCHEMA, "events": events })
            .to_string()
            .as_bytes(),
    )
}

impl Harness {
    async fn post(&self, token: Option<&str>, body: Vec<u8>) -> (StatusCode, Value) {
        let mut req = Request::post("/v1/events")
            .header("content-type", "application/json")
            .header("content-encoding", "gzip");
        if let Some(token) = token {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        let res = self
            .app
            .clone()
            .oneshot(req.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn send(&self, events: &[Event]) -> (StatusCode, Value) {
        self.post(Some(&self.token), batch(events)).await
    }

    /// Distinct events stored for this tenant, as a reader sees them.
    async fn stored(&self) -> u64 {
        self.db
            .api
            .tenant(self.tenant)
            .query("SELECT uniqExact(event_id) FROM events WHERE tenant_id = {tenant:UUID}")
            .unwrap()
            .fetch_one()
            .await
            .unwrap()
    }
}

fn model_call(tokens: u64) -> Event {
    let mut e = testing::event("t1", EventKind::ModelCompleted, Utc::now());
    e.set(attr::GEN_AI_OUTPUT_TOKENS, tokens);
    e
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn stores_a_batch_and_redelivery_keeps_one_newest_copy() {
    let h = harness(&[]).await;
    let start = testing::event("t1", EventKind::AgentStarted, Utc::now());
    let mut model = model_call(1);
    let (status, body) = h.send(&[start.clone(), model.clone()]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "stored": 2 }));

    // gsd resends the batch, then an updated model call.
    h.send(&[start, model.clone()]).await;
    model.set(attr::GEN_AI_OUTPUT_TOKENS, 42);
    h.send(&[model.clone()]).await;
    assert_eq!(h.stored().await, 2);

    let newest: u64 =
        h.db.api
            .tenant(h.tenant)
            .query(
                "SELECT argMax(output_tokens, received_at) FROM events
             WHERE tenant_id = {tenant:UUID} AND event_id = {id:UUID}",
            )
            .unwrap()
            .param("id", model.id.to_string())
            .fetch_one()
            .await
            .unwrap();
    assert_eq!(newest, 42);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn a_bad_event_is_refused_and_the_rest_stored() {
    let h = harness(&[]).await;
    let good = testing::event("t1", EventKind::AgentStarted, Utc::now());
    let future = testing::event("t1", EventKind::TurnUser, Utc::now() + Duration::days(2));
    let (status, body) = h.send(&[good, future.clone()]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["stored"], 1);
    assert_eq!(body["rejected"][0]["id"], future.id.to_string());
    assert_eq!(
        body["rejected"][0]["error"],
        "timestamp is more than 24h in the future"
    );
    assert_eq!(h.stored().await, 1);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn a_timestamp_storage_cannot_hold_refuses_that_event_only() {
    let h = harness(&[]).await;
    let good = testing::event("t1", EventKind::AgentStarted, Utc::now());
    // What a client sends when it never set the time.
    let unset = testing::event(
        "t1",
        EventKind::TurnUser,
        "0001-01-01T00:00:00Z".parse().unwrap(),
    );
    let (status, body) = h.send(&[good, unset.clone()]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["stored"], 1);
    assert_eq!(body["rejected"][0]["id"], unset.id.to_string());
    assert_eq!(body["rejected"][0]["error"], "timestamp is before 1900");
    assert_eq!(h.stored().await, 1);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn made_up_tokens_cost_one_lookup_each_and_lookups_are_limited() {
    // Two lookups at once, then one a second.
    let h = harness(&["--lookup-rate", "1"]).await;
    let events = [testing::event("t1", EventKind::AgentStarted, Utc::now())];
    assert_eq!(h.send(&events).await.0, StatusCode::OK);
    let (first, second) = (made_up(), made_up());
    assert_eq!(
        h.post(Some(&first), batch(&events)).await.0,
        StatusCode::UNAUTHORIZED
    );
    // The allowance is spent: another unseen token waits...
    assert_eq!(
        h.post(Some(&second), batch(&events)).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    // ...while tokens already seen, known or not, are answered from memory.
    assert_eq!(
        h.post(Some(&first), batch(&events)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(h.send(&events).await.0, StatusCode::OK);
    // A token that isn't shaped like one never reaches Postgres, so it is
    // refused, not told to wait.
    for misshapen in ["not-a-token", "gsi_made_up"] {
        assert_eq!(
            h.post(Some(misshapen), batch(&events)).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn refuses_requests_it_cannot_attribute_or_read() {
    let h = harness(&[]).await;
    let events = [testing::event("t1", EventKind::AgentStarted, Utc::now())];
    assert_eq!(
        h.post(None, batch(&events)).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        h.post(Some("gsi_nope"), batch(&events)).await.0,
        StatusCode::UNAUTHORIZED
    );

    let unknown = gzip(br#"{"schema":"groundstation.telemetry.v9","events":[]}"#);
    assert_eq!(
        h.post(Some(&h.token), unknown).await.0,
        StatusCode::BAD_REQUEST
    );
    let no_id = gzip(br#"{"schema":"groundstation.telemetry.v0","events":[{"id":1}]}"#);
    assert_eq!(
        h.post(Some(&h.token), no_id).await.0,
        StatusCode::BAD_REQUEST
    );

    // Too large as sent, and too large once inflated.
    let huge = vec![b' '; 9 << 20];
    let req = Request::post("/v1/events")
        .header("authorization", format!("Bearer {}", h.token))
        .body(Body::from(huge))
        .unwrap();
    let status = h.app.clone().oneshot(req).await.unwrap().status();
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    let bomb = gzip(&vec![b' '; 65 << 20]);
    assert_eq!(
        h.post(Some(&h.token), bomb).await.0,
        StatusCode::PAYLOAD_TOO_LARGE
    );

    // A suspended tenant's token is known but refused.
    let suspended = testing::tenant(&h.db.pg).await;
    let (_, token) = tokens::create(&h.db.pg, suspended.id, "t", None)
        .await
        .unwrap();
    tenants::set_suspended(&h.db.pg, suspended.id, true)
        .await
        .unwrap();
    assert_eq!(
        h.post(Some(&token), batch(&events)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(h.stored().await, 0);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn rate_limits_per_token_with_retry_after() {
    let h = harness(&["--token-burst", "1", "--token-rate", "0.5"]).await;
    let events = [testing::event("t1", EventKind::AgentStarted, Utc::now())];
    assert_eq!(h.send(&events).await.0, StatusCode::OK);
    let req = Request::post("/v1/events")
        .header("authorization", format!("Bearer {}", h.token))
        .header("content-encoding", "gzip")
        .body(Body::from(batch(&events)))
        .unwrap();
    let res = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(res.headers()["retry-after"], "2");
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn healthz_reports_both_databases() {
    let h = harness(&[]).await;
    let req = Request::get("/healthz").body(Body::empty()).unwrap();
    let res = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn a_token_in_use_is_checked_again_even_while_made_up_ones_flood_in() {
    // Answers kept for a second; two lookups at once, then one a second.
    let h = harness(&["--lookup-rate", "1", "--token-cache-secs", "1"]).await;
    let events = [testing::event("t1", EventKind::AgentStarted, Utc::now())];
    assert_eq!(h.send(&events).await.0, StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    // Made-up tokens take every lookup there is...
    let mut busy = false;
    for _ in 0..4 {
        let status = h.post(Some(&made_up()), batch(&events)).await.0;
        busy |= status == StatusCode::TOO_MANY_REQUESTS;
    }
    assert!(busy, "the flood used up the lookups");
    // ...and the real token, its answer now a second old, still gets through.
    assert_eq!(h.send(&events).await.0, StatusCode::OK);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn a_batch_of_more_events_than_it_holds_is_too_large() {
    let h = harness(&[]).await;
    let one = testing::event("t1", EventKind::AgentStarted, Utc::now());
    // gsd halves a batch that gets 413.
    let many = vec![one; 10_001];
    assert_eq!(h.send(&many).await.0, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(h.send(&many[..10_000]).await.0, StatusCode::OK);
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn storage_down_is_a_503_that_refuses_no_event() {
    // Nothing listens on this port.
    let h = harness(&["--clickhouse-url", "http://127.0.0.1:9"]).await;
    let events = [testing::event("t1", EventKind::AgentStarted, Utc::now())];
    let (status, body) = h.send(&events).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body.get("rejected").is_none(), "{body}");
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn a_retention_past_what_clickhouse_dates_keeps_events_until_then() {
    let h = harness(&[]).await;
    let forever = tenants::create(&h.db.pg, "forever", 40_000).await.unwrap();
    let (_, token) = tokens::create(&h.db.pg, forever.id, "t", None)
        .await
        .unwrap();
    let event = testing::event("t1", EventKind::AgentStarted, Utc::now());
    let (status, body) = h.post(Some(&token), batch(&[event])).await;
    assert_eq!(
        (status, &body["stored"]),
        (StatusCode::OK, &json!(1)),
        "{body}"
    );
    let until: String = Clickhouse::connect(&h.db.clickhouse_args, Service::Api)
        .tenant(forever.id)
        .query("SELECT toString(retain_until) FROM events WHERE tenant_id = {tenant:UUID}")
        .unwrap()
        .fetch_one()
        .await
        .unwrap();
    assert_eq!(until, "2106-02-07 06:28:15");
}

/// An upload whose body never finishes arriving.
fn stalled(token: &str) -> Request<Body> {
    let stream = futures_util::stream::once(async {
        Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"{"))
    })
    .chain(futures_util::stream::pending());
    Request::post("/v1/events")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from_stream(stream))
        .unwrap()
}

/// Four stalled uploads, all one decoding slot holds: they end when the
/// read timeout cuts them off.
async fn hold_every_body(
    h: &Harness,
) -> Vec<tokio::task::JoinHandle<Result<axum::response::Response, std::convert::Infallible>>> {
    let held = (0..4)
        .map(|_| tokio::spawn(h.app.clone().oneshot(stalled(&h.token))))
        .collect();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    held
}

async fn cut_off(
    held: Vec<tokio::task::JoinHandle<Result<axum::response::Response, std::convert::Infallible>>>,
) {
    for request in held {
        let res = tokio::time::timeout(std::time::Duration::from_secs(10), request)
            .await
            .expect("a stalled body is cut off")
            .unwrap()
            .unwrap();
        assert_eq!(res.status(), StatusCode::REQUEST_TIMEOUT);
    }
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn a_body_that_stalls_is_cut_off_and_bodies_held_are_bounded() {
    // One decoding slot, so four bodies held at once; a second to send one.
    let h = harness(&["--decoding", "1", "--read-timeout-secs", "1"]).await;
    let held = hold_every_body(&h).await;
    let busy = h.app.clone().oneshot(stalled(&h.token)).await.unwrap();
    assert_eq!(busy.status(), StatusCode::TOO_MANY_REQUESTS);
    cut_off(held).await;
}

#[tokio::test]
#[ignore = "needs `docker compose up -d --wait`"]
async fn refused_as_busy_costs_the_client_none_of_its_quota() {
    // Five requests' worth, refilled too slowly to matter here.
    let h = harness(&[
        "--decoding",
        "1",
        "--read-timeout-secs",
        "1",
        "--token-burst",
        "5",
        "--token-rate",
        "0.001",
    ])
    .await;
    let held = hold_every_body(&h).await;
    for _ in 0..3 {
        let busy = h.app.clone().oneshot(stalled(&h.token)).await.unwrap();
        assert_eq!(busy.status(), StatusCode::TOO_MANY_REQUESTS);
    }
    cut_off(held).await;
    // Refused for the instance's load, not its own: its fifth request is here.
    let events = [testing::event("t1", EventKind::AgentStarted, Utc::now())];
    let (status, body) = h.send(&events).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}
