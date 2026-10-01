//! Ships stored events to the backend in gzip-compressed batches, retrying
//! with backoff. The local store is the queue, so an unavailable backend only
//! delays uploads; it never loses events or slows agents down.
//!
//! The backend can refuse single events while accepting the rest of a batch
//! (`IngestResponse::rejected`). A refused event goes back in the queue and is
//! dropped from it after [`MAX_ATTEMPTS`] refusals, so one bad event can't
//! hold up everything behind it. Dropped events stay in the local store.

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use flate2::Compression;
use flate2::write::GzEncoder;
use groundstation_api::{IngestResponse, UploadError};
use groundstation_schema::{Batch, Event};
use reqwest::StatusCode;
use reqwest::header::RETRY_AFTER;
use tokio::sync::watch;
use uuid::Uuid;

use crate::config::{Config, PathPolicy};
use crate::outbound;
use crate::store::Store;

const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// Refusals an event gets before it leaves the upload queue.
pub const MAX_ATTEMPTS: i64 = 3;

/// The most recent upload failure, cleared by the next success. Shared with
/// the HTTP server so `/v1/health` can say why events are piling up.
#[derive(Clone, Default)]
pub struct UploadState(Arc<Mutex<Option<UploadError>>>);

impl UploadState {
    pub fn last_error(&self) -> Option<UploadError> {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<UploadError>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn record(&self, result: &Result<usize, Failure>) {
        match result {
            Ok(_) => *self.lock() = None,
            Err(Failure::Transient { error, .. }) => {
                *self.lock() = Some(UploadError {
                    message: format!("{error:#}"),
                    at: Utc::now(),
                })
            }
            // Handled by splitting the batch; not a failure anyone needs to see.
            Err(Failure::TooLarge) => {}
        }
    }
}

/// Uploads until shutdown. Returns immediately in `local-only` mode.
pub async fn run(
    config: Arc<Config>,
    store: Arc<Store>,
    state: UploadState,
    mut shutdown: watch::Receiver<bool>,
) {
    let Some(endpoint) = config.upload_endpoint() else {
        return;
    };
    let transport = &config.transport;
    let token = transport.token.as_deref();
    let batch_size = transport.batch_size.max(1);
    let url = format!("{}/v1/events", endpoint.trim_end_matches('/'));
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("gsd/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("uploader disabled: {e:#}");
            return;
        }
    };
    let paths = config.redaction.paths;
    let interval = Duration::from_secs(transport.flush_interval_secs.max(1));
    let mut backoff = interval;
    // Shrinks when the backend says a batch is too large, grows back on success.
    let mut limit = batch_size;
    tracing::info!(%url, "uploading events");

    loop {
        let result = flush(&client, &url, token, &store, limit, paths).await;
        state.record(&result);
        let wait = match result {
            Ok(sent) => {
                backoff = interval;
                let full = sent == limit;
                limit = (limit * 2).min(batch_size);
                if full { Duration::ZERO } else { interval } // full: more waiting
            }
            Err(Failure::TooLarge) => {
                limit = (limit / 2).max(1);
                tracing::debug!(limit, "backend refused the batch as too large, splitting");
                Duration::ZERO
            }
            Err(Failure::Transient { error, retry_after }) => {
                let wait = retry_after.unwrap_or(backoff);
                tracing::warn!("upload failed, retrying in {wait:?}: {error:#}");
                backoff = (backoff * 2).min(MAX_BACKOFF);
                wait
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = shutdown.changed() => {
                // One last attempt so a clean shutdown doesn't strand a batch.
                let _ = flush(&client, &url, token, &store, limit, paths).await;
                return;
            }
        }
    }
}

#[derive(Debug)]
enum Failure {
    /// 413 for a batch of more than one event: retry with fewer.
    TooLarge,
    /// Network errors, auth, rate limits, server errors: nothing wrong with
    /// the events themselves, so they stay queued and nothing counts against them.
    Transient {
        error: anyhow::Error,
        retry_after: Option<Duration>,
    },
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Self::Transient {
            error,
            retry_after: None,
        }
    }
}

/// Sends the oldest `limit` queued events. Returns how many left the queue's
/// head: accepted, or refused and counted against.
async fn flush(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
    store: &Arc<Store>,
    limit: usize,
    paths: PathPolicy,
) -> Result<usize, Failure> {
    let s = store.clone();
    let pending = blocking(move || s.pending_upload(limit.max(1))).await?;
    if pending.is_empty() {
        return Ok(0);
    }
    let sent = pending.len();
    let mut seqs: HashMap<Uuid, i64> = pending.iter().map(|p| (p.event.id, p.seq)).collect();
    let events = pending
        .into_iter()
        .map(|mut p| {
            outbound::prepare(&mut p.event, &p.place, paths);
            p.event
        })
        .collect();
    let body = encode(events)?;

    let mut req = client
        .post(url)
        .header("content-type", "application/json")
        .header("content-encoding", "gzip")
        .body(body);
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await.context("sending batch")?;
    let status = resp.status();

    let rejected: Vec<(Uuid, String)> = if status == StatusCode::PAYLOAD_TOO_LARGE {
        if sent > 1 {
            return Err(Failure::TooLarge);
        }
        // One event the backend won't take at any batch size.
        seqs.keys()
            .map(|id| (*id, "too large for the backend".to_string()))
            .collect()
    } else if status.is_success() {
        // A backend that doesn't itemize refusals accepted everything.
        let reply: IngestResponse = resp.json().await.unwrap_or_default();
        reply
            .rejected
            .into_iter()
            .map(|r| (r.id, r.error))
            .collect()
    } else {
        let retry_after = (status == StatusCode::TOO_MANY_REQUESTS)
            .then(|| resp.headers().get(RETRY_AFTER)?.to_str().ok()?.parse().ok())
            .flatten()
            .map(Duration::from_secs);
        let text = resp.text().await.unwrap_or_default();
        return Err(Failure::Transient {
            error: anyhow!("backend returned {status}: {}", clip(&text)),
            retry_after,
        });
    };

    let mut refused = Vec::with_capacity(rejected.len());
    for (id, error) in &rejected {
        if let Some(seq) = seqs.remove(id) {
            tracing::warn!(event = %id, error = %clip(error), "backend refused event");
            refused.push(seq);
        }
    }
    let accepted: Vec<i64> = seqs.into_values().collect();
    let s = store.clone();
    let dropped = blocking(move || {
        s.mark_uploaded(&accepted)?;
        s.record_rejections(&refused, MAX_ATTEMPTS)
    })
    .await?;
    if !dropped.is_empty() {
        tracing::warn!(
            events = dropped.len(),
            "dropped events from the upload queue after {MAX_ATTEMPTS} refusals; they stay in the local store"
        );
    }
    tracing::debug!(events = sent, refused = rejected.len(), "uploaded batch");
    Ok(sent)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f).await?
}

fn encode(events: Vec<Event>) -> Result<Vec<u8>> {
    gzip(&serde_json::to_vec(&Batch::new(events))?)
}

/// Backend messages are logged, so keep them short.
fn clip(s: &str) -> String {
    s.chars().take(200).collect()
}

fn gzip(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut enc = GzEncoder::new(Vec::with_capacity(bytes.len() / 4), Compression::default());
    enc.write_all(bytes)?;
    Ok(enc.finish()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewEvent;
    use axum::body::Bytes;
    use axum::http::HeaderMap;
    use axum::response::IntoResponse;
    use chrono::Utc;
    use flate2::read::GzDecoder;
    use groundstation_api::Rejection;
    use groundstation_schema::{Agent, EventKind, attr};
    use std::io::Read;
    use std::sync::Mutex;

    type Received = Arc<Mutex<Vec<(Option<String>, Batch)>>>;

    /// Serves `/v1/events`, records each batch, and answers with `reply`.
    async fn backend(
        reply: impl Fn(&Batch) -> axum::response::Response + Clone + Send + Sync + 'static,
    ) -> (String, Received) {
        let received: Received = Arc::default();
        let sink = received.clone();
        let app = axum::Router::new().route(
            "/v1/events",
            axum::routing::post(move |headers: HeaderMap, body: Bytes| {
                let (sink, reply) = (sink.clone(), reply.clone());
                async move {
                    let mut json = Vec::new();
                    GzDecoder::new(&body[..]).read_to_end(&mut json).unwrap();
                    let auth = headers
                        .get("authorization")
                        .map(|v| v.to_str().unwrap().to_string());
                    let batch: Batch = serde_json::from_slice(&json).unwrap();
                    let response = reply(&batch);
                    sink.lock().unwrap().push((auth, batch));
                    response
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/v1/events"), received)
    }

    fn store_with(n: usize) -> (Arc<Store>, Vec<Uuid>) {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let events: Vec<NewEvent> = (0..n)
            .map(|_| NewEvent {
                event: Event::new(
                    Uuid::now_v7(),
                    "t",
                    EventKind::ToolStarted,
                    Utc::now(),
                    Agent::named("x"),
                ),
                raw: None,
                host: None,
            })
            .collect();
        let ids = events.iter().map(|e| e.event.id).collect();
        store.insert(events).unwrap();
        (store, ids)
    }

    #[tokio::test]
    async fn uploads_compressed_batches_and_marks_them() {
        let (url, received) = backend(|_| "ok".into_response()).await;
        let (store, _) = store_with(3);
        let client = reqwest::Client::new();

        assert_eq!(
            flush(&client, &url, Some("tok"), &store, 2, PathPolicy::Keep)
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            flush(&client, &url, Some("tok"), &store, 2, PathPolicy::Keep)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            flush(&client, &url, Some("tok"), &store, 2, PathPolicy::Keep)
                .await
                .unwrap(),
            0
        );

        let received = received.lock().unwrap();
        assert_eq!(received.len(), 2);
        assert_eq!(received[0].0.as_deref(), Some("Bearer tok"));
        assert_eq!(received[0].1.schema, groundstation_schema::SCHEMA);
        assert_eq!(store.counts().unwrap().pending_upload, 0);
    }

    #[tokio::test]
    async fn drops_an_event_refused_three_times_and_keeps_the_rest() {
        let (store, ids) = store_with(3);
        let bad = ids[0];
        let (url, received) = backend(move |batch| {
            let rejected = batch
                .events
                .iter()
                .filter(|e| e.id == bad)
                .map(|e| Rejection {
                    id: e.id,
                    error: "nope".into(),
                })
                .collect();
            axum::Json(IngestResponse {
                stored: 0,
                rejected,
            })
            .into_response()
        })
        .await;
        let client = reqwest::Client::new();

        // The refused event goes back in the queue; the others are done.
        flush(&client, &url, None, &store, 10, PathPolicy::Keep)
            .await
            .unwrap();
        assert_eq!(store.counts().unwrap().pending_upload, 1);
        for _ in 1..MAX_ATTEMPTS {
            flush(&client, &url, None, &store, 10, PathPolicy::Keep)
                .await
                .unwrap();
        }
        let counts = store.counts().unwrap();
        assert_eq!(counts.pending_upload, 0);
        assert_eq!(counts.dropped_upload, 1);
        assert_eq!(counts.events, 3, "dropped events stay in the store");
        assert_eq!(
            flush(&client, &url, None, &store, 10, PathPolicy::Keep)
                .await
                .unwrap(),
            0
        );
        assert_eq!(received.lock().unwrap().len(), MAX_ATTEMPTS as usize);
    }

    #[tokio::test]
    async fn splits_batches_the_backend_finds_too_large() {
        let (url, _) = backend(|batch| {
            if batch.events.len() > 1 {
                StatusCode::PAYLOAD_TOO_LARGE.into_response()
            } else {
                "ok".into_response()
            }
        })
        .await;
        let (store, _) = store_with(2);
        let client = reqwest::Client::new();

        assert!(matches!(
            flush(&client, &url, None, &store, 2, PathPolicy::Keep).await,
            Err(Failure::TooLarge)
        ));
        assert_eq!(
            flush(&client, &url, None, &store, 1, PathPolicy::Keep)
                .await
                .unwrap(),
            1
        );
        assert_eq!(store.counts().unwrap().pending_upload, 1);
    }

    #[tokio::test]
    async fn server_errors_count_against_no_event() {
        let (url, _) = backend(|_| StatusCode::SERVICE_UNAVAILABLE.into_response()).await;
        let (store, _) = store_with(1);
        let client = reqwest::Client::new();

        for _ in 0..MAX_ATTEMPTS + 1 {
            assert!(matches!(
                flush(&client, &url, None, &store, 10, PathPolicy::Keep).await,
                Err(Failure::Transient { .. })
            ));
        }
        let counts = store.counts().unwrap();
        assert_eq!((counts.pending_upload, counts.dropped_upload), (1, 0));
    }

    #[tokio::test]
    async fn remembers_the_last_failure_until_an_upload_succeeds() {
        let (url, _) = backend(|_| StatusCode::UNAUTHORIZED.into_response()).await;
        let (store, _) = store_with(1);
        let client = reqwest::Client::new();
        let state = UploadState::default();

        state.record(&flush(&client, &url, None, &store, 10, PathPolicy::Keep).await);
        let error = state.last_error().expect("a failure is recorded");
        assert!(error.message.contains("401"), "{}", error.message);

        // Splitting a batch isn't a failure; it leaves the last error alone.
        state.record(&Err(Failure::TooLarge));
        assert!(state.last_error().is_some());
        state.record(&Ok(1));
        assert_eq!(state.last_error(), None);
    }

    #[tokio::test]
    async fn uploads_say_where_within_the_repository_not_on_disk() {
        let (url, received) = backend(|_| "ok".into_response()).await;
        let store = Arc::new(Store::open_in_memory().unwrap());
        let mut event = Event::new(
            Uuid::now_v7(),
            "t",
            EventKind::ToolStarted,
            Utc::now(),
            Agent::named("x"),
        );
        event.set(attr::CWD, "/Users/ana/src/shop");
        event.set(attr::FILE_PATH, "/Users/ana/src/shop/src/lib.rs");
        store
            .insert(vec![NewEvent {
                event,
                raw: None,
                host: Some("ana-mbp".into()),
            }])
            .unwrap();
        store
            .set_repository("t", "/Users/ana/src/shop", Some("github.com/acme/shop"))
            .unwrap();

        flush(
            &reqwest::Client::new(),
            &url,
            None,
            &store,
            10,
            PathPolicy::Keep,
        )
        .await
        .unwrap();
        let received = received.lock().unwrap();
        let sent = &received[0].1.events[0];
        assert_eq!(
            sent.get(attr::FILE_PATH),
            Some(&serde_json::json!("src/lib.rs"))
        );
        assert_eq!(sent.get(attr::CWD), Some(&serde_json::json!(".")));
        assert_eq!(
            sent.get(attr::VCS_REPOSITORY),
            Some(&serde_json::json!("github.com/acme/shop"))
        );
        assert_eq!(sent.get(attr::HOST_NAME), None, "the hostname stays home");
        // The local copy keeps the full path for the local UI.
        let (_, events) = store.trajectory("t").unwrap().unwrap();
        assert_eq!(
            events[0].get(attr::FILE_PATH),
            Some(&serde_json::json!("/Users/ana/src/shop/src/lib.rs"))
        );
    }

    #[tokio::test]
    async fn honors_retry_after_when_rate_limited() {
        let (url, _) =
            backend(|_| (StatusCode::TOO_MANY_REQUESTS, [(RETRY_AFTER, "7")]).into_response())
                .await;
        let (store, _) = store_with(1);
        let client = reqwest::Client::new();

        match flush(&client, &url, None, &store, 10, PathPolicy::Keep).await {
            Err(Failure::Transient { retry_after, .. }) => {
                assert_eq!(retry_after, Some(Duration::from_secs(7)))
            }
            other => panic!("expected a transient failure, got {other:?}"),
        }
    }
}
