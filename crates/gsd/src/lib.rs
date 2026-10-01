//! `gsd`, the Ground Station local daemon.
//!
//! Agents send events to gsd on `127.0.0.1:4318`: raw hook payloads, which
//! gsd translates with the agent's adapter (see `adapters/` in the
//! repository), or finished `groundstation.telemetry.v0` batches from SDKs.
//! gsd normalizes them to `groundstation.telemetry.v0`, applies privacy
//! rules, persists them locally, serves them back for local inspection and,
//! when a backend is configured, uploads them in compressed batches.

pub mod config;
pub mod ingest;
pub mod perms;
pub mod privacy;
pub mod server;
pub mod spool;
pub mod store;
pub mod ui;
pub mod uploader;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::watch;

use crate::config::Config;
use crate::ingest::Ingestor;
use crate::privacy::Privacy;
use crate::server::AppState;
use crate::store::Store;

const SPOOL_INTERVAL: Duration = Duration::from_secs(2);

/// Runs the daemon until SIGINT/SIGTERM or `POST /v1/shutdown`.
pub async fn run(config: Config) -> Result<()> {
    let config = Arc::new(config);
    let data_dir = config.data_dir();
    perms::create_dir_all(&data_dir)?;

    let store = Arc::new(Store::open(&config.db_path())?);
    let privacy = Privacy::new(&config)?;
    let ingestor = Arc::new(Ingestor::new(store.clone(), privacy));

    let listener = tokio::net::TcpListener::bind(config.daemon.listen)
        .await
        .with_context(|| {
            format!(
                "binding {} (is gsd already running? try `groundstation status`)",
                config.daemon.listen
            )
        })?;
    let pid_file = config.pid_file();
    perms::write(&pid_file, std::process::id().to_string().as_bytes())?;

    let requeued = spool::requeue(&config.spool_dir(), &ingestor);
    if requeued > 0 {
        tracing::info!(
            payloads = requeued,
            "retrying payloads an older gsd couldn't handle"
        );
    }

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut tasks = tokio::task::JoinSet::new();
    tasks.spawn(drain_spool(
        config.spool_dir(),
        ingestor.clone(),
        shutdown_rx.clone(),
    ));
    tasks.spawn(uploader::run(
        config.clone(),
        store.clone(),
        shutdown_rx.clone(),
    ));

    let app = server::router(AppState {
        ingestor,
        config: config.clone(),
        shutdown: shutdown_tx.clone(),
    });
    tracing::info!(
        listen = %config.daemon.listen,
        data_dir = %data_dir.display(),
        transport = config.transport.mode.as_str(),
        upload = config.upload_endpoint().unwrap_or("-"),
        "gsd {} ready",
        env!("CARGO_PKG_VERSION"),
    );

    let mut server_shutdown = shutdown_rx.clone();
    tokio::spawn(async move {
        wait_for_signal().await;
        let _ = shutdown_tx.send(true);
    });
    let served = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = server_shutdown.wait_for(|stop| *stop).await;
            tracing::info!("shutting down");
        })
        .await;

    while tasks.join_next().await.is_some() {}
    let _ = std::fs::remove_file(&pid_file);
    served.context("serving")
}

async fn drain_spool(
    dir: std::path::PathBuf,
    ingestor: Arc<Ingestor>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        let (d, i) = (dir.clone(), ingestor.clone());
        match tokio::task::spawn_blocking(move || spool::drain(&d, &i)).await {
            Ok(n) if n > 0 => tracing::info!(events = n, "ingested spooled events"),
            Ok(_) => {}
            Err(e) => tracing::error!("spool drain panicked: {e}"),
        }
        tokio::select! {
            _ = tokio::time::sleep(SPOOL_INTERVAL) => {}
            _ = shutdown.wait_for(|stop| *stop) => return,
        }
    }
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("installing SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

/// Logging for binaries: `RUST_LOG` wins, otherwise `info` for gsd. Colors
/// only when writing to a terminal, so `gsd.log` stays plain text.
pub fn init_tracing() {
    use std::io::IsTerminal;
    use tracing_subscriber::EnvFilter;
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("gsd=info,warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(std::io::stdout().is_terminal())
        .init();
}
