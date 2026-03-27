use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::capture::CaptureEvent;
use super::connection::{handle_connection, CorrelateState, ImplicitCaptureState};
use super::pool::SessionPool;
use super::socket;

/// Run the TCP accept loop.
pub async fn run_listener(
    listener: TcpListener,
    pool: Arc<SessionPool>,
    capture_tx: mpsc::UnboundedSender<CaptureEvent>,
    no_capture: Arc<AtomicBool>,
    metrics_tx: Option<mpsc::UnboundedSender<CaptureEvent>>,
    enable_correlation: bool,
    implicit_capture: Option<Arc<ImplicitCaptureState>>,
) -> Result<()> {
    let session_counter = AtomicU64::new(1);
    let addr = listener.local_addr()?;
    info!("Proxy listening on {addr}");

    loop {
        let (client_stream, peer_addr) = listener.accept().await?;

        // Apply socket hardening (keepalive, nodelay) to client connection
        if let Err(e) = socket::configure_socket(&client_stream) {
            warn!("Failed to configure client socket from {peer_addr}: {e}");
        }

        let session_id = session_counter.fetch_add(1, Ordering::Relaxed);
        let pool = pool.clone();
        let capture_tx = capture_tx.clone();
        let metrics_tx = metrics_tx.clone();
        let no_capture = no_capture.clone();
        let implicit_capture = implicit_capture.clone();

        // Each connection gets its own CorrelateState (per-connection queue)
        // Enable correlation if either explicit correlation or implicit capture is active
        let correlate = if enable_correlation || implicit_capture.is_some() {
            Some(Arc::new(CorrelateState::new()))
        } else {
            None
        };

        info!("Session {session_id}: accepted connection from {peer_addr}");

        tokio::spawn(async move {
            handle_connection(
                client_stream,
                pool,
                session_id,
                capture_tx,
                no_capture,
                metrics_tx,
                correlate,
                implicit_capture,
            )
            .await;
        });
    }
}
