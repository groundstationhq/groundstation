//! gsd's local HTTP API.
//!
//! The daemon holds prompts and source code, so it only answers requests
//! addressed to a loopback host name (blocking DNS-rebinding reads from web
//! pages) and only accepts `application/json` bodies (which browsers cannot
//! send cross-origin without a CORS preflight that gsd never grants).

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use groundstation_api::{
    ErrorBody, Health, HookEnvelope, IngestResponse, ResyncResponse, ToolStats, TrajectoryDetail,
    TrajectorySummary, UploadStatus,
};
use groundstation_schema::{Batch, SCHEMA};
use serde::Deserialize;
use tokio::sync::watch;

use crate::config::Config;
use crate::ingest::Ingestor;
use crate::store::Resolved;

#[derive(Clone)]
pub struct AppState {
    pub ingestor: Arc<Ingestor>,
    pub config: Arc<Config>,
    pub shutdown: watch::Sender<bool>,
}

pub fn router(state: AppState) -> Router {
    let listen = state.config.daemon.listen;
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/events", post(ingest_events))
        .route("/v1/adapters/{name}", post(ingest_hook))
        .route("/v1/adapters/{name}/resync", post(resync))
        .route("/v1/trajectories", get(list_trajectories))
        .route("/v1/trajectories/{id}", get(get_trajectory))
        .route("/v1/stats/tools", get(tool_stats))
        .route("/v1/shutdown", post(shutdown))
        .fallback(crate::ui::serve)
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(middleware::from_fn(move |req, next| {
            check_host(listen, req, next)
        }))
        .with_state(state)
}

async fn check_host(listen: SocketAddr, req: Request, next: Next) -> Response {
    if listen.ip().is_loopback() {
        let host = req
            .headers()
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("");
        let name = host.rsplit_once(':').map_or(host, |(name, port)| {
            if port.chars().all(|c| c.is_ascii_digit()) {
                name
            } else {
                host
            }
        });
        if !matches!(name, "127.0.0.1" | "localhost" | "[::1]") {
            return error(StatusCode::FORBIDDEN, format!("host {host:?} not allowed"))
                .into_response();
        }
    }
    next.run(req).await
}

async fn health(State(s): State<AppState>) -> Result<Json<Health>, ApiError> {
    let store = s.ingestor.store.clone();
    let counts = blocking(move || store.counts()).await?;
    Ok(Json(Health {
        status: "ok".into(),
        ui: crate::ui::EMBEDDED,
        adapters: s
            .ingestor
            .adapter_names()
            .into_iter()
            .map(String::from)
            .collect(),
        version: env!("CARGO_PKG_VERSION").into(),
        schema: SCHEMA.into(),
        pid: std::process::id(),
        data_dir: s.config.data_dir().display().to_string(),
        trajectories: counts.trajectories,
        events: counts.events,
        spool_pending: crate::spool::pending(&s.config.spool_dir()).len(),
        upload: UploadStatus {
            mode: s.config.transport.mode.as_str().into(),
            endpoint: s.config.upload_endpoint().map(String::from),
            pending: counts.pending_upload,
            dropped: counts.dropped_upload,
        },
    }))
}

async fn ingest_events(
    State(s): State<AppState>,
    Json(batch): Json<Batch>,
) -> Result<Json<IngestResponse>, ApiError> {
    let ingestor = s.ingestor.clone();
    let stored = blocking(move || ingestor.ingest_batch(batch)).await?;
    Ok(Json(IngestResponse {
        stored,
        ..Default::default()
    }))
}

async fn ingest_hook(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Json(envelope): Json<HookEnvelope>,
) -> Result<Json<IngestResponse>, ApiError> {
    if !s.ingestor.has_adapter(&name) {
        return Err(error(
            StatusCode::NOT_FOUND,
            format!(
                "unknown adapter {name:?}; this gsd supports: {}",
                s.ingestor.adapter_names().join(", ")
            ),
        ));
    }
    let ingestor = s.ingestor.clone();
    let (stored, follow_up) = blocking(move || ingestor.ingest_hook(&name, envelope)).await?;
    // Reply now: the hook sits in the agent's critical path (Codex caps some
    // hooks at 3s), while a first transcript read can take much longer.
    let ingestor = s.ingestor.clone();
    tokio::task::spawn_blocking(move || ingestor.follow_up(follow_up));
    Ok(Json(IngestResponse {
        stored,
        ..Default::default()
    }))
}

async fn resync(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Json(_): Json<serde_json::Value>,
) -> Result<Json<ResyncResponse>, ApiError> {
    if !s.ingestor.has_adapter(&name) {
        return Err(error(
            StatusCode::NOT_FOUND,
            format!(
                "unknown adapter {name:?}; this gsd supports: {}",
                s.ingestor.adapter_names().join(", ")
            ),
        ));
    }
    let ingestor = s.ingestor.clone();
    Ok(Json(blocking(move || ingestor.resync(&name)).await?))
}

#[derive(Deserialize)]
struct ListParams {
    limit: Option<usize>,
}

async fn list_trajectories(
    State(s): State<AppState>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<TrajectorySummary>>, ApiError> {
    let store = s.ingestor.store.clone();
    let limit = params.limit.unwrap_or(50).clamp(1, 10_000);
    Ok(Json(
        blocking(move || store.list_trajectories(limit)).await?,
    ))
}

async fn get_trajectory(
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<TrajectoryDetail>, ApiError> {
    let store = s.ingestor.store.clone();
    let found = blocking(move || {
        Ok(match store.resolve_trajectory(&id)? {
            Resolved::Found(id) => Ok(store.trajectory(&id)?),
            Resolved::NotFound => Ok(None),
            Resolved::Ambiguous(ids) => Err(ids),
        })
    })
    .await?;
    match found {
        Ok(Some((summary, events))) => Ok(Json(TrajectoryDetail { summary, events })),
        Ok(None) => Err(error(StatusCode::NOT_FOUND, "trajectory not found")),
        Err(ids) => Err(error(
            StatusCode::CONFLICT,
            format!("ambiguous id, matches: {}", ids.join(", ")),
        )),
    }
}

#[derive(Deserialize)]
struct StatsParams {
    days: Option<u32>,
}

async fn tool_stats(
    State(s): State<AppState>,
    Query(params): Query<StatsParams>,
) -> Result<Json<ToolStats>, ApiError> {
    let days = params.days.unwrap_or(14).clamp(1, 365);
    let since = chrono::Utc::now() - chrono::Duration::days(i64::from(days));
    let since_ns = since.timestamp_nanos_opt().unwrap_or(0);
    let store = s.ingestor.store.clone();
    let tools = blocking(move || store.tool_latency(since_ns)).await?;
    Ok(Json(ToolStats { days, since, tools }))
}

async fn shutdown(State(s): State<AppState>, Json(_): Json<serde_json::Value>) -> StatusCode {
    let _ = s.shutdown.send(true);
    StatusCode::ACCEPTED
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, ApiError> {
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => {
            tracing::warn!("request failed: {e:#}");
            Err(error(StatusCode::UNPROCESSABLE_ENTITY, format!("{e:#}")))
        }
        Err(e) => Err(error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    }
}

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(ErrorBody { error: self.1 })).into_response()
    }
}

fn error(status: StatusCode, message: impl Into<String>) -> ApiError {
    ApiError(status, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::privacy::Privacy;
    use crate::store::Store;
    use axum::body::Body;
    use chrono::Utc;
    use groundstation_schema::{Agent, Event, EventKind, attr};
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tower::ServiceExt;
    use uuid::Uuid;

    fn app() -> Router {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let ingestor = Arc::new(Ingestor::new(
            store,
            Privacy::with_env(&Config::default(), []).unwrap(),
        ));
        let (shutdown, _) = watch::channel(false);
        router(AppState {
            ingestor,
            config: Arc::new(Config::default()),
            shutdown,
        })
    }

    async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        host: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", host);
        if body.is_some() {
            req = req.header("content-type", "application/json");
        }
        let body = body.map_or(Body::empty(), |b| Body::from(b.to_string()));
        let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[tokio::test]
    async fn resync_by_adapter() {
        let app = app();
        let host = "127.0.0.1:4318";
        let (status, body) = call(
            &app,
            "POST",
            "/v1/adapters/claude-code/resync",
            host,
            Some(json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let typed: ResyncResponse = serde_json::from_value(body).unwrap();
        assert_eq!(typed.transcripts, 0);

        let (status, body) = call(
            &app,
            "POST",
            "/v1/adapters/nope/resync",
            host,
            Some(json!({})),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"].as_str().unwrap().contains("claude-code"));
    }

    #[tokio::test]
    async fn hook_to_trajectory() {
        let app = app();
        let host = "127.0.0.1:4318";
        let envelope = json!({
            "id": Uuid::now_v7(), "observed_at": Utc::now(),
            "payload": {"session_id": "abc123", "hook_event_name": "UserPromptSubmit", "prompt": "hello"},
        });
        let (status, body) = call(
            &app,
            "POST",
            "/v1/adapters/claude-code",
            host,
            Some(envelope),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["stored"], 1);

        let (_, list) = call(&app, "GET", "/v1/trajectories", "localhost:4318", None).await;
        assert_eq!(list[0]["id"], "abc123");
        assert_eq!(list[0]["title"], "hello");

        let (status, detail) = call(&app, "GET", "/v1/trajectories/abc", host, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["events"][0]["kind"], "turn.user");
        let typed: TrajectoryDetail = serde_json::from_value(detail).unwrap();
        assert_eq!(typed.summary.event_count, 1);

        let (status, _) = call(&app, "GET", "/v1/trajectories/zzz", host, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let envelope = json!({"id": Uuid::now_v7(), "observed_at": Utc::now(), "payload": {}});
        let (status, body) = call(&app, "POST", "/v1/adapters/nope", host, Some(envelope)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            body["error"].as_str().unwrap().contains("claude-code"),
            "{body}"
        );
    }

    #[tokio::test]
    async fn tool_stats_window() {
        let app = app();
        let host = "127.0.0.1:4318";
        let mut started = Event::new(
            Uuid::now_v7(),
            "t",
            EventKind::ToolStarted,
            Utc::now(),
            Agent::named("x"),
        )
        .with_span("s1");
        started.set(attr::TOOL_CATEGORY, "shell");
        let mut done = Event::new(
            Uuid::now_v7(),
            "t",
            EventKind::ToolCompleted,
            Utc::now(),
            Agent::named("x"),
        )
        .with_span("s1");
        done.set(attr::TOOL_CATEGORY, "shell");
        done.set(attr::DURATION_MS, 42);
        let batch = json!({"schema": SCHEMA, "events": [started, done]});
        let (status, _) = call(&app, "POST", "/v1/events", host, Some(batch)).await;
        assert_eq!(status, StatusCode::OK);

        let (status, body) = call(&app, "GET", "/v1/stats/tools?days=7", host, None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let stats: ToolStats = serde_json::from_value(body).unwrap();
        assert_eq!(stats.days, 7);
        assert_eq!(stats.tools.len(), 1);
        assert_eq!(
            (
                stats.tools[0].category.as_str(),
                stats.tools[0].calls,
                stats.tools[0].p50_ms
            ),
            ("shell", 1, 42)
        );

        let (_, body) = call(&app, "GET", "/v1/stats/tools?days=9999", host, None).await;
        assert_eq!(body["days"], 365);
    }

    #[tokio::test]
    async fn rejects_foreign_hosts_and_non_json() {
        let app = app();
        let (status, _) = call(&app, "GET", "/v1/trajectories", "evil.example:4318", None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let req = Request::builder()
            .method("POST")
            .uri("/v1/events")
            .header("host", "127.0.0.1:4318")
            .header("content-type", "text/plain")
            .body(Body::from(
                r#"{"schema":"groundstation.telemetry.v0","events":[]}"#,
            ))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
    }

    #[tokio::test]
    async fn native_events_require_known_schema() {
        let app = app();
        let batch = json!({"schema": "something.else", "events": []});
        let (status, _) = call(&app, "POST", "/v1/events", "127.0.0.1", Some(batch)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

        let batch = json!({"schema": SCHEMA, "events": [{
            "id": Uuid::now_v7(), "trajectory_id": "custom-1", "kind": "agent.started",
            "timestamp": Utc::now(), "agent": {"name": "my-agent", "version": "1.0"}
        }]});
        let (status, body) = call(&app, "POST", "/v1/events", "127.0.0.1", Some(batch)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["stored"], 1);
    }
}
