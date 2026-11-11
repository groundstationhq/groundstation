//! What every service does the same way: logs, serves HTTP and stops cleanly.

use std::io::IsTerminal;
use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use tracing_subscriber::EnvFilter;

/// Logging: `RUST_LOG` wins, otherwise `default` (e.g. `groundstation_api=info,warn`).
/// Colors only on a terminal.
pub fn init_tracing(default: &str) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(std::io::stdout().is_terminal())
        .init();
}

/// How long requests in flight get to finish once the service is asked to
/// stop: under the 30 s most orchestrators wait before killing a process.
const GRACE: Duration = Duration::from_secs(25);

/// Serves `app` on `listen` until SIGINT or SIGTERM, then lets in-flight
/// requests finish, for a while.
pub async fn serve(name: &str, listen: SocketAddr, app: Router) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(listen).await?;
    tracing::info!(listen = %listener.local_addr()?, "{name} {} ready", env!("CARGO_PKG_VERSION"));
    serve_until(listener, app, shutdown_signal(), GRACE).await
}

/// Serves until `stop` resolves, then waits up to `grace` for open requests.
/// One that is still open then (a stalled upload, say) is cut off: its client
/// sees the connection close, never a success, and tries again.
async fn serve_until(
    listener: tokio::net::TcpListener,
    app: Router,
    stop: impl Future<Output = ()> + Send + 'static,
    grace: Duration,
) -> std::io::Result<()> {
    let (stopping, mut stopped) = tokio::sync::watch::channel(false);
    let server = axum::serve(listener, app).with_graceful_shutdown(async move {
        stop.await;
        tracing::info!("shutting down");
        let _ = stopping.send(true);
    });
    tokio::select! {
        served = server => served,
        () = async {
            let _ = stopped.wait_for(|stopping| *stopping).await;
            tokio::time::sleep(grace).await;
        } => {
            tracing::warn!("requests still open after {grace:?}; stopping anyway");
            Ok(())
        }
    }
}

/// Resolves on SIGINT or SIGTERM.
pub async fn shutdown_signal() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_stalled_request_cannot_hold_up_stopping() {
        let app = Router::new().route(
            "/",
            axum::routing::get(std::future::pending::<&'static str>),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(serve_until(
            listener,
            app,
            async {
                let _ = stopped.await;
            },
            Duration::from_millis(200),
        ));
        // A request that never completes.
        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        tokio::io::AsyncWriteExt::write_all(&mut client, b"GET / HTTP/1.1\r\nhost: x\r\n\r\n")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        stop.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("the service stops after its grace period")
            .unwrap()
            .unwrap();
    }
}
