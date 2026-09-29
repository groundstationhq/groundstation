//! Ships stored events to the backend in gzip-compressed batches, retrying
//! with backoff. The local store is the queue, so an unavailable backend only
//! delays uploads; it never loses events or slows agents down.

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use flate2::Compression;
use flate2::write::GzEncoder;
use groundstation_schema::Batch;
use tokio::sync::watch;

use crate::config::Config;
use crate::store::Store;

const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// Uploads until shutdown. Returns immediately in `local-only` mode.
pub async fn run(config: Arc<Config>, store: Arc<Store>, mut shutdown: watch::Receiver<bool>) {
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
    let interval = Duration::from_secs(transport.flush_interval_secs.max(1));
    let mut backoff = interval;
    tracing::info!(%url, "uploading events");

    loop {
        let wait = match flush(&client, &url, token, &store, batch_size).await {
            Ok(sent) if sent == batch_size => Duration::ZERO, // more waiting
            Ok(_) => {
                backoff = interval;
                interval
            }
            Err(e) => {
                tracing::warn!("upload failed, retrying in {backoff:?}: {e:#}");
                let wait = backoff;
                backoff = (backoff * 2).min(MAX_BACKOFF);
                wait
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            _ = shutdown.changed() => {
                // One last attempt so a clean shutdown doesn't strand a batch.
                let _ = flush(&client, &url, token, &store, batch_size).await;
                return;
            }
        }
    }
}

async fn flush(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
    store: &Arc<Store>,
    batch_size: usize,
) -> Result<usize> {
    let s = store.clone();
    let pending =
        tokio::task::spawn_blocking(move || s.pending_upload(batch_size.max(1))).await??;
    if pending.is_empty() {
        return Ok(0);
    }
    let (seqs, events): (Vec<i64>, Vec<_>) = pending.into_iter().unzip();
    let body = gzip(&serde_json::to_vec(&Batch::new(events))?)?;

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
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!(
            "backend returned {status}: {}",
            text.chars().take(200).collect::<String>()
        );
    }
    let n = seqs.len();
    let s = store.clone();
    tokio::task::spawn_blocking(move || s.mark_uploaded(&seqs)).await??;
    tracing::debug!(events = n, "uploaded batch");
    Ok(n)
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
    use chrono::Utc;
    use flate2::read::GzDecoder;
    use groundstation_schema::{Agent, Event, EventKind};
    use std::io::Read;
    use std::sync::Mutex;
    use uuid::Uuid;

    #[tokio::test]
    async fn uploads_compressed_batches_and_marks_them() {
        let received = Arc::new(Mutex::new(Vec::<(Option<String>, Batch)>::new()));
        let sink = received.clone();
        let app = axum::Router::new().route(
            "/v1/events",
            axum::routing::post(move |headers: HeaderMap, body: Bytes| {
                let sink = sink.clone();
                async move {
                    let mut json = Vec::new();
                    GzDecoder::new(&body[..]).read_to_end(&mut json).unwrap();
                    let auth = headers
                        .get("authorization")
                        .map(|v| v.to_str().unwrap().to_string());
                    sink.lock()
                        .unwrap()
                        .push((auth, serde_json::from_slice(&json).unwrap()));
                    "ok"
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let store = Arc::new(Store::open_in_memory().unwrap());
        let events = (0..3)
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
        store.insert(events).unwrap();

        let client = reqwest::Client::new();
        let url = format!("http://{addr}/v1/events");
        assert_eq!(
            flush(&client, &url, Some("tok"), &store, 2).await.unwrap(),
            2
        );
        assert_eq!(
            flush(&client, &url, Some("tok"), &store, 2).await.unwrap(),
            1
        );
        assert_eq!(
            flush(&client, &url, Some("tok"), &store, 2).await.unwrap(),
            0
        );

        let received = received.lock().unwrap();
        assert_eq!(received.len(), 2);
        assert_eq!(received[0].0.as_deref(), Some("Bearer tok"));
        assert_eq!(received[0].1.schema, groundstation_schema::SCHEMA);
        assert_eq!(store.counts().unwrap().pending_upload, 0);
    }
}
