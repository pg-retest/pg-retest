use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tracing::{info, warn};

use super::capture::CaptureEvent;
use super::connection::{handle_connection, CorrelateState, ImplicitCaptureState, TimeoutConfig};
use super::metrics::ProxyMetrics;
use super::pool::SessionPool;
use super::protocol::build_error_response;
use super::socket;

/// Per-IP concurrent connection counter.
pub type IpConnectionMap = Arc<DashMap<IpAddr, AtomicU32>>;

/// RAII guard that decrements the per-IP connection counter when dropped.
struct IpConnectionGuard {
    ip: IpAddr,
    map: IpConnectionMap,
}

impl Drop for IpConnectionGuard {
    fn drop(&mut self) {
        if let Some(entry) = self.map.get(&self.ip) {
            let prev = entry.value().fetch_sub(1, Ordering::Relaxed);
            // If we decremented to zero, try to remove the entry to keep the map clean.
            // Use remove_if to avoid TOCTOU races: only remove if still zero.
            if prev == 1 {
                drop(entry); // release the read ref before removing
                self.map
                    .remove_if(&self.ip, |_, v| v.load(Ordering::Relaxed) == 0);
            }
        }
    }
}

/// RAII guard that decrements `connections_active` on the metrics struct
/// when dropped. Paired with `record_accept()` at the point of acceptance
/// so the active-connection gauge stays balanced even on early returns.
struct MetricsActiveGuard {
    metrics: Option<Arc<ProxyMetrics>>,
}

impl Drop for MetricsActiveGuard {
    fn drop(&mut self) {
        if let Some(m) = &self.metrics {
            m.record_close();
        }
    }
}

/// Run the TCP accept loop.
#[allow(clippy::too_many_arguments)]
pub async fn run_listener(
    listener: TcpListener,
    pool: Arc<SessionPool>,
    capture_tx: mpsc::UnboundedSender<CaptureEvent>,
    no_capture: Arc<AtomicBool>,
    metrics_tx: Option<mpsc::UnboundedSender<CaptureEvent>>,
    enable_correlation: bool,
    implicit_capture: Option<Arc<ImplicitCaptureState>>,
    timeouts: TimeoutConfig,
    max_connections_per_ip: u32,
    tls_acceptor: Option<Arc<TlsAcceptor>>,
    proxy_metrics: Option<Arc<ProxyMetrics>>,
) -> Result<()> {
    let session_counter = AtomicU64::new(1);
    let addr = listener.local_addr()?;
    info!("Proxy listening on {addr}");

    // Per-IP connection tracking (only allocated if limit is set)
    let ip_map: IpConnectionMap = Arc::new(DashMap::new());

    loop {
        let (mut client_stream, peer_addr) = listener.accept().await?;

        // Apply socket hardening (keepalive, nodelay) to client connection
        if let Err(e) = socket::configure_socket(&client_stream) {
            warn!("Failed to configure client socket from {peer_addr}: {e}");
        }

        // Per-IP connection limit check
        if max_connections_per_ip > 0 {
            let ip = peer_addr.ip();
            let current = ip_map
                .entry(ip)
                .or_insert_with(|| AtomicU32::new(0))
                .value()
                .fetch_add(1, Ordering::Relaxed)
                + 1;

            if current > max_connections_per_ip {
                // Over limit — decrement counter, send error, close
                if let Some(entry) = ip_map.get(&ip) {
                    entry.value().fetch_sub(1, Ordering::Relaxed);
                }
                if let Some(m) = &proxy_metrics {
                    m.record_reject_per_ip();
                }
                warn!(
                    "Connection from {peer_addr} rejected: {current} connections exceeds \
                     per-IP limit of {max_connections_per_ip}"
                );
                let err = build_error_response(
                    "FATAL",
                    "53300",
                    &format!("too many connections from {ip} ({max_connections_per_ip} max)"),
                );
                let _ = client_stream.write_all(&err).await;
                let _ = client_stream.shutdown().await;
                continue;
            }
        }

        // Accept counted after all rejection paths so we don't inflate numbers
        // with connections that were refused before ever running a handler.
        if let Some(m) = &proxy_metrics {
            m.record_accept();
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

        let timeouts = timeouts.clone();
        let tls_acceptor = tls_acceptor.clone();

        // Create the IP guard if tracking is active — it will decrement on drop
        let ip_guard = if max_connections_per_ip > 0 {
            Some(IpConnectionGuard {
                ip: peer_addr.ip(),
                map: ip_map.clone(),
            })
        } else {
            None
        };

        let metrics_guard = MetricsActiveGuard {
            metrics: proxy_metrics.clone(),
        };

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
                timeouts,
                tls_acceptor,
            )
            .await;
            // ip_guard and metrics_guard are dropped here, decrementing counters
            drop(ip_guard);
            drop(metrics_guard);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ip_connection_guard_increments_and_decrements() {
        let map: IpConnectionMap = Arc::new(DashMap::new());
        let ip: IpAddr = "127.0.0.1".parse().unwrap();

        // Simulate incrementing
        map.entry(ip)
            .or_insert_with(|| AtomicU32::new(0))
            .value()
            .fetch_add(1, Ordering::Relaxed);

        assert_eq!(map.get(&ip).unwrap().load(Ordering::Relaxed), 1);

        // Create guard and drop it
        {
            let _guard = IpConnectionGuard {
                ip,
                map: map.clone(),
            };
        }
        // After drop, count should be 0 and entry removed
        assert!(map.get(&ip).is_none());
    }

    #[test]
    fn test_ip_connection_guard_multiple_connections() {
        let map: IpConnectionMap = Arc::new(DashMap::new());
        let ip: IpAddr = "10.0.0.1".parse().unwrap();

        // Simulate 3 connections
        for _ in 0..3 {
            map.entry(ip)
                .or_insert_with(|| AtomicU32::new(0))
                .value()
                .fetch_add(1, Ordering::Relaxed);
        }
        assert_eq!(map.get(&ip).unwrap().load(Ordering::Relaxed), 3);

        // Drop one guard — count should go to 2
        {
            let _guard = IpConnectionGuard {
                ip,
                map: map.clone(),
            };
        }
        assert_eq!(map.get(&ip).unwrap().load(Ordering::Relaxed), 2);

        // Drop another
        {
            let _guard = IpConnectionGuard {
                ip,
                map: map.clone(),
            };
        }
        assert_eq!(map.get(&ip).unwrap().load(Ordering::Relaxed), 1);

        // Drop last — entry should be removed
        {
            let _guard = IpConnectionGuard {
                ip,
                map: map.clone(),
            };
        }
        assert!(map.get(&ip).is_none());
    }

    #[test]
    fn test_ip_connection_guard_different_ips() {
        let map: IpConnectionMap = Arc::new(DashMap::new());
        let ip1: IpAddr = "192.168.1.1".parse().unwrap();
        let ip2: IpAddr = "192.168.1.2".parse().unwrap();

        // Increment both
        for ip in [ip1, ip2] {
            map.entry(ip)
                .or_insert_with(|| AtomicU32::new(0))
                .value()
                .fetch_add(1, Ordering::Relaxed);
        }

        // Drop guard for ip1 only
        {
            let _guard = IpConnectionGuard {
                ip: ip1,
                map: map.clone(),
            };
        }

        // ip1 should be gone, ip2 should remain
        assert!(map.get(&ip1).is_none());
        assert_eq!(map.get(&ip2).unwrap().load(Ordering::Relaxed), 1);
    }
}
