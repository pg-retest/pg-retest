//! Backend health check for the pg-retest proxy.
//!
//! Runs a periodic TCP-level connectivity probe against the proxy's upstream
//! target. Credential-free by design: the proxy passes client authentication
//! straight through to the backend, so it has no database user of its own to
//! authenticate with. What it *can* do is verify that the target TCP endpoint
//! is accepting connections and speaking the PostgreSQL protocol's SSLRequest
//! handshake. If a fresh TCP connect plus an SSLRequest round trip fails N
//! times in a row, the proxy flips its `backend_degraded` metric and logs a
//! warning so operators have something to alert on.
//!
//! This is intentionally simpler than sending `SELECT 1`. Running `SELECT 1`
//! would require a pre-authenticated connection, which the proxy can't create
//! without credentials it deliberately doesn't hold. The TCP + SSLRequest
//! check catches the failure modes operators actually care about: backend is
//! down, listening port is firewalled, PG is stuck during startup, DNS broke.
//! It does not catch "PG is up but `SELECT 1` returns an error," which is a
//! rare failure mode that pg_stat_statements would catch anyway.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::metrics::ProxyMetrics;

/// Configuration for the health check background task.
#[derive(Debug, Clone)]
pub struct HealthCheckConfig {
    /// Interval between checks in seconds. 0 disables health checks entirely.
    pub interval_secs: u64,
    /// Per-check timeout in seconds (TCP connect + handshake).
    pub timeout_secs: u64,
    /// Consecutive failures before flipping the `backend_degraded` flag.
    pub fail_threshold: u64,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            interval_secs: 30,
            timeout_secs: 5,
            fail_threshold: 3,
        }
    }
}

/// Perform a single TCP + SSLRequest probe against the target.
///
/// Sends the 8-byte PG SSLRequest packet and expects a single byte back
/// (`'S'` if the server supports TLS, `'N'` if not). Either reply means
/// PG is accepting connections, which is what we want to verify. Anything
/// else (including a disconnect without a reply) is a failure.
pub async fn probe_target(target: &str, timeout_secs: u64) -> Result<(), String> {
    let dur = Duration::from_secs(timeout_secs);

    // Connect with a timeout
    let stream_fut = TcpStream::connect(target);
    let mut stream = match timeout(dur, stream_fut).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(format!("connect failed: {e}")),
        Err(_) => return Err(format!("connect timed out after {timeout_secs}s")),
    };

    // Send PG SSLRequest: 8 bytes, length=8 as i32 BE, then magic 80877103 as i32 BE
    let mut ssl_req = [0u8; 8];
    ssl_req[0..4].copy_from_slice(&8_i32.to_be_bytes());
    ssl_req[4..8].copy_from_slice(&80877103_i32.to_be_bytes());

    match timeout(dur, stream.write_all(&ssl_req)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(format!("write failed: {e}")),
        Err(_) => return Err(format!("write timed out after {timeout_secs}s")),
    }

    // Read a single byte reply. PG responds with 'S' (TLS supported) or 'N'
    // (TLS not supported). Either one means the backend is alive.
    let mut buf = [0u8; 1];
    match timeout(dur, stream.read_exact(&mut buf)).await {
        Ok(Ok(_)) => {
            if buf[0] == b'S' || buf[0] == b'N' {
                // Graceful close — don't care if the shutdown itself fails
                let _ = stream.shutdown().await;
                Ok(())
            } else {
                Err(format!(
                    "unexpected SSLRequest reply byte: 0x{:02x}",
                    buf[0]
                ))
            }
        }
        Ok(Err(e)) => Err(format!("read failed: {e}")),
        Err(_) => Err(format!("read timed out after {timeout_secs}s")),
    }
}

/// Spawn a background task that probes the target on an interval and
/// updates the ProxyMetrics health fields. Returns None if health checks
/// are disabled (interval_secs == 0).
pub fn spawn_health_check(
    target: String,
    metrics: Arc<ProxyMetrics>,
    config: HealthCheckConfig,
    cancel: CancellationToken,
) -> Option<tokio::task::JoinHandle<()>> {
    if config.interval_secs == 0 {
        debug!("Health check disabled (interval=0)");
        return None;
    }

    let interval_secs = config.interval_secs;
    let timeout_secs = config.timeout_secs;
    let threshold = config.fail_threshold.max(1);

    info!("Health check: probing {target} every {interval_secs}s (fail threshold: {threshold})");

    let handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
        // Skip the initial immediate tick so we don't probe before the proxy
        // has actually started accepting connections.
        ticker.tick().await;

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    match probe_target(&target, timeout_secs).await {
                        Ok(()) => {
                            let was_degraded = metrics.backend_degraded.load(
                                std::sync::atomic::Ordering::Relaxed,
                            );
                            metrics.record_healthcheck_ok();
                            if was_degraded {
                                info!("Health check: backend {target} recovered");
                            } else {
                                debug!("Health check: backend {target} ok");
                            }
                        }
                        Err(reason) => {
                            let consecutive = metrics.record_healthcheck_fail();
                            if consecutive >= threshold {
                                let was_degraded = metrics.backend_degraded.load(
                                    std::sync::atomic::Ordering::Relaxed,
                                );
                                metrics.mark_degraded();
                                if !was_degraded {
                                    warn!(
                                        "Health check: backend {target} unreachable for \
                                         {consecutive} consecutive probes — flipping \
                                         degraded flag. Last error: {reason}"
                                    );
                                } else {
                                    debug!(
                                        "Health check: backend {target} still unreachable \
                                         ({consecutive} failures). {reason}"
                                    );
                                }
                            } else {
                                debug!(
                                    "Health check: backend {target} probe failed \
                                     ({consecutive}/{threshold}): {reason}"
                                );
                            }
                        }
                    }
                }
                _ = cancel.cancelled() => {
                    debug!("Health check: shutting down");
                    break;
                }
            }
        }
    });

    Some(handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn test_default_config() {
        let c = HealthCheckConfig::default();
        assert_eq!(c.interval_secs, 30);
        assert_eq!(c.timeout_secs, 5);
        assert_eq!(c.fail_threshold, 3);
    }

    #[tokio::test]
    async fn test_probe_fails_on_unreachable_target() {
        // Pick a port nothing should be listening on
        let result = probe_target("127.0.0.1:1", 2).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_probe_fails_on_invalid_target() {
        let result = probe_target("not-a-valid-hostname-xyz.invalid:5432", 2).await;
        assert!(result.is_err());
    }

    /// Spawn a fake PG server that accepts one connection, reads 8 bytes, and
    /// replies with 'N' (no TLS). This is the minimum conformant SSLRequest
    /// response that pg-retest's probe should treat as "backend healthy."
    async fn fake_pg_no_tls() -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let handle = tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 8];
                let _ = stream.read_exact(&mut buf).await;
                let _ = stream.write_all(b"N").await;
                let _ = stream.shutdown().await;
            }
        });
        (addr, handle)
    }

    /// Fake PG server that sends 'S' (TLS supported).
    async fn fake_pg_tls() -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let handle = tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 8];
                let _ = stream.read_exact(&mut buf).await;
                let _ = stream.write_all(b"S").await;
                let _ = stream.shutdown().await;
            }
        });
        (addr, handle)
    }

    /// Fake server that accepts the connection then immediately drops it
    /// without replying. This should register as a probe failure.
    async fn fake_drop_on_connect() -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let handle = tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                drop(stream);
            }
        });
        (addr, handle)
    }

    #[tokio::test]
    async fn test_probe_succeeds_against_fake_pg_no_tls() {
        let (addr, _h) = fake_pg_no_tls().await;
        let result = probe_target(&addr, 2).await;
        assert!(result.is_ok(), "probe failed: {:?}", result);
    }

    #[tokio::test]
    async fn test_probe_succeeds_against_fake_pg_tls() {
        let (addr, _h) = fake_pg_tls().await;
        let result = probe_target(&addr, 2).await;
        assert!(result.is_ok(), "probe failed: {:?}", result);
    }

    #[tokio::test]
    async fn test_probe_fails_when_server_drops_connection() {
        let (addr, _h) = fake_drop_on_connect().await;
        let result = probe_target(&addr, 2).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_spawn_health_check_disabled_returns_none() {
        let metrics = Arc::new(ProxyMetrics::new());
        let cancel = CancellationToken::new();
        let handle = spawn_health_check(
            "127.0.0.1:5432".to_string(),
            metrics,
            HealthCheckConfig {
                interval_secs: 0,
                timeout_secs: 5,
                fail_threshold: 3,
            },
            cancel,
        );
        assert!(handle.is_none());
    }

    #[tokio::test]
    async fn test_spawn_health_check_enabled_returns_handle() {
        let metrics = Arc::new(ProxyMetrics::new());
        let cancel = CancellationToken::new();
        let handle = spawn_health_check(
            "127.0.0.1:1".to_string(),
            metrics,
            HealthCheckConfig {
                interval_secs: 60,
                timeout_secs: 2,
                fail_threshold: 3,
            },
            cancel.clone(),
        );
        assert!(handle.is_some());
        cancel.cancel();
        handle.unwrap().await.unwrap();
    }

    #[tokio::test]
    async fn test_health_check_flips_degraded_after_threshold() {
        // Point at a closed port so every probe fails. Short interval to keep
        // the test fast, threshold 2 so we only need two failed probes.
        let metrics = Arc::new(ProxyMetrics::new());
        let cancel = CancellationToken::new();
        let handle = spawn_health_check(
            "127.0.0.1:1".to_string(),
            Arc::clone(&metrics),
            HealthCheckConfig {
                interval_secs: 1,
                timeout_secs: 1,
                fail_threshold: 2,
            },
            cancel.clone(),
        )
        .unwrap();

        // Wait long enough for at least two failed probes (2 * interval_secs + a bit of slack)
        tokio::time::sleep(Duration::from_millis(2500)).await;

        assert!(
            metrics
                .backend_degraded
                .load(std::sync::atomic::Ordering::Relaxed),
            "backend_degraded should be true after {} failed probes",
            metrics
                .backend_consecutive_failures
                .load(std::sync::atomic::Ordering::Relaxed)
        );
        assert!(
            metrics
                .backend_healthchecks_fail_total
                .load(std::sync::atomic::Ordering::Relaxed)
                >= 2
        );

        cancel.cancel();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_health_check_recovers_when_backend_comes_back() {
        // Start a fake PG, run a few successful probes, then stop the fake
        // PG and verify the probe transitions to failure.
        // Simplified: just verify one successful probe clears state.
        let metrics = Arc::new(ProxyMetrics::new());
        metrics.record_healthcheck_fail();
        metrics.record_healthcheck_fail();
        metrics.mark_degraded();

        let (addr, _h) = fake_pg_no_tls().await;
        let result = probe_target(&addr, 2).await;
        assert!(result.is_ok());
        metrics.record_healthcheck_ok();

        assert!(!metrics
            .backend_degraded
            .load(std::sync::atomic::Ordering::Relaxed));
        assert_eq!(
            metrics
                .backend_consecutive_failures
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
    }
}
