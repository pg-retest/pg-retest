//! Prometheus-shaped metrics for the pg-retest proxy.
//!
//! Holds a set of atomic counters and gauges that are updated at key points
//! in the listener, connection handler, and pool. Rendered via `render()` as
//! Prometheus text-exposition format and served from the `/metrics` endpoint
//! on the control HTTP server.
//!
//! The counters are hand-rolled `AtomicU64` fields rather than pulled from a
//! `metrics` / `metrics-exporter-prometheus` crate because (a) the counter
//! surface is small and stable, (b) the proxy already pulls in a lot of
//! dependencies, and (c) a single render function is easier to audit than a
//! registry-driven exporter.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// Aggregate metrics for a running proxy instance.
pub struct ProxyMetrics {
    /// When the metrics were initialized (proxy startup).
    pub started_at: Instant,
    /// Total client connections accepted on the listener.
    pub connections_total: AtomicU64,
    /// Currently-active client connections (accepted minus closed).
    pub connections_active: AtomicU64,
    /// Connections rejected by the per-IP cap.
    pub connections_rejected_per_ip: AtomicU64,
    /// Connections rejected due to oversized PG protocol messages.
    pub connections_rejected_msg_size: AtomicU64,
    /// Connections rejected for other reasons (auth timeout, client closed, etc).
    pub connections_rejected_other: AtomicU64,
    /// Total PG queries relayed to the backend.
    pub queries_total: AtomicU64,
    /// Total bytes relayed from client to server.
    pub bytes_in_total: AtomicU64,
    /// Total bytes relayed from server to client.
    pub bytes_out_total: AtomicU64,
    /// Total errors encountered at any stage of a connection lifecycle.
    pub errors_total: AtomicU64,
    /// Whether the backend health check has flipped the degraded flag.
    /// 0 = healthy (or check disabled), 1 = degraded.
    pub backend_degraded: AtomicBool,
    /// Consecutive health check failures since last success.
    pub backend_consecutive_failures: AtomicU64,
    /// Total successful health checks since startup (0 if disabled).
    pub backend_healthchecks_ok_total: AtomicU64,
    /// Total failed health checks since startup (0 if disabled).
    pub backend_healthchecks_fail_total: AtomicU64,
}

impl ProxyMetrics {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            connections_total: AtomicU64::new(0),
            connections_active: AtomicU64::new(0),
            connections_rejected_per_ip: AtomicU64::new(0),
            connections_rejected_msg_size: AtomicU64::new(0),
            connections_rejected_other: AtomicU64::new(0),
            queries_total: AtomicU64::new(0),
            bytes_in_total: AtomicU64::new(0),
            bytes_out_total: AtomicU64::new(0),
            errors_total: AtomicU64::new(0),
            backend_degraded: AtomicBool::new(false),
            backend_consecutive_failures: AtomicU64::new(0),
            backend_healthchecks_ok_total: AtomicU64::new(0),
            backend_healthchecks_fail_total: AtomicU64::new(0),
        }
    }

    /// Record a successful accept on the listener.
    pub fn record_accept(&self) {
        self.connections_total.fetch_add(1, Ordering::Relaxed);
        self.connections_active.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a connection closing (regardless of whether it ended cleanly).
    pub fn record_close(&self) {
        self.connections_active.fetch_sub(1, Ordering::Relaxed);
    }

    /// Record a per-IP rejection.
    pub fn record_reject_per_ip(&self) {
        self.connections_rejected_per_ip
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record a message-size rejection.
    pub fn record_reject_msg_size(&self) {
        self.connections_rejected_msg_size
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record a miscellaneous rejection.
    pub fn record_reject_other(&self) {
        self.connections_rejected_other
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Record a query relayed to the backend.
    pub fn record_query(&self) {
        self.queries_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Record `n` bytes relayed from client to server.
    pub fn record_bytes_in(&self, n: u64) {
        self.bytes_in_total.fetch_add(n, Ordering::Relaxed);
    }

    /// Record `n` bytes relayed from server to client.
    pub fn record_bytes_out(&self, n: u64) {
        self.bytes_out_total.fetch_add(n, Ordering::Relaxed);
    }

    /// Record a connection-level error.
    pub fn record_error(&self) {
        self.errors_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a successful backend health check.
    pub fn record_healthcheck_ok(&self) {
        self.backend_healthchecks_ok_total
            .fetch_add(1, Ordering::Relaxed);
        self.backend_consecutive_failures
            .store(0, Ordering::Relaxed);
        self.backend_degraded.store(false, Ordering::Relaxed);
    }

    /// Record a failed backend health check. Returns the new consecutive failure count.
    pub fn record_healthcheck_fail(&self) -> u64 {
        self.backend_healthchecks_fail_total
            .fetch_add(1, Ordering::Relaxed);
        self.backend_consecutive_failures
            .fetch_add(1, Ordering::Relaxed)
            + 1
    }

    /// Flip the degraded flag (called by the health check task after N consecutive failures).
    pub fn mark_degraded(&self) {
        self.backend_degraded.store(true, Ordering::Relaxed);
    }

    /// Render the metrics in Prometheus text-exposition format.
    /// `pool_active` and `pool_idle` are passed in because they come from the
    /// SessionPool, not from the metrics struct directly.
    pub fn render(&self, pool_active: usize, pool_idle: usize) -> String {
        let uptime_seconds = self.started_at.elapsed().as_secs();
        let degraded = if self.backend_degraded.load(Ordering::Relaxed) {
            1
        } else {
            0
        };

        let mut out = String::new();
        // Proxy lifecycle
        push_counter(
            &mut out,
            "pg_retest_proxy_connections_total",
            "Total client connections accepted",
            self.connections_total.load(Ordering::Relaxed),
        );
        push_gauge(
            &mut out,
            "pg_retest_proxy_connections_active",
            "Client connections currently open",
            self.connections_active.load(Ordering::Relaxed),
        );
        push_labeled_counter(
            &mut out,
            "pg_retest_proxy_connections_rejected_total",
            "Connections rejected by resource limits",
            &[
                (
                    "reason",
                    "per_ip",
                    self.connections_rejected_per_ip.load(Ordering::Relaxed),
                ),
                (
                    "reason",
                    "msg_size",
                    self.connections_rejected_msg_size.load(Ordering::Relaxed),
                ),
                (
                    "reason",
                    "other",
                    self.connections_rejected_other.load(Ordering::Relaxed),
                ),
            ],
        );

        // Traffic
        push_counter(
            &mut out,
            "pg_retest_proxy_queries_total",
            "Total PG queries relayed to the backend",
            self.queries_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut out,
            "pg_retest_proxy_bytes_in_total",
            "Total bytes relayed from client to server",
            self.bytes_in_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut out,
            "pg_retest_proxy_bytes_out_total",
            "Total bytes relayed from server to client",
            self.bytes_out_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut out,
            "pg_retest_proxy_errors_total",
            "Total connection-level errors",
            self.errors_total.load(Ordering::Relaxed),
        );

        // Pool
        push_gauge(
            &mut out,
            "pg_retest_proxy_pool_active",
            "Server connections currently checked out",
            pool_active as u64,
        );
        push_gauge(
            &mut out,
            "pg_retest_proxy_pool_idle",
            "Server connections sitting idle in the pool",
            pool_idle as u64,
        );

        // Backend health
        push_gauge(
            &mut out,
            "pg_retest_proxy_backend_degraded",
            "1 if health checks detected the backend as unreachable, 0 otherwise",
            degraded,
        );
        push_counter(
            &mut out,
            "pg_retest_proxy_backend_healthchecks_ok_total",
            "Total successful backend health checks",
            self.backend_healthchecks_ok_total.load(Ordering::Relaxed),
        );
        push_counter(
            &mut out,
            "pg_retest_proxy_backend_healthchecks_fail_total",
            "Total failed backend health checks",
            self.backend_healthchecks_fail_total.load(Ordering::Relaxed),
        );

        // Uptime
        push_gauge(
            &mut out,
            "pg_retest_proxy_uptime_seconds",
            "Seconds since proxy startup",
            uptime_seconds,
        );

        out
    }
}

impl Default for ProxyMetrics {
    fn default() -> Self {
        Self::new()
    }
}

fn push_counter(out: &mut String, name: &str, help: &str, value: u64) {
    out.push_str(&format!("# HELP {name} {help}\n"));
    out.push_str(&format!("# TYPE {name} counter\n"));
    out.push_str(&format!("{name} {value}\n"));
}

fn push_gauge(out: &mut String, name: &str, help: &str, value: u64) {
    out.push_str(&format!("# HELP {name} {help}\n"));
    out.push_str(&format!("# TYPE {name} gauge\n"));
    out.push_str(&format!("{name} {value}\n"));
}

fn push_labeled_counter(
    out: &mut String,
    name: &str,
    help: &str,
    labeled_values: &[(&str, &str, u64)],
) {
    out.push_str(&format!("# HELP {name} {help}\n"));
    out.push_str(&format!("# TYPE {name} counter\n"));
    for (label, val, n) in labeled_values {
        out.push_str(&format!("{name}{{{label}=\"{val}\"}} {n}\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_counters_start_zero() {
        let m = ProxyMetrics::new();
        assert_eq!(m.connections_total.load(Ordering::Relaxed), 0);
        assert_eq!(m.connections_active.load(Ordering::Relaxed), 0);
        assert_eq!(m.queries_total.load(Ordering::Relaxed), 0);
        assert_eq!(m.bytes_in_total.load(Ordering::Relaxed), 0);
        assert_eq!(m.bytes_out_total.load(Ordering::Relaxed), 0);
        assert_eq!(m.errors_total.load(Ordering::Relaxed), 0);
        assert!(!m.backend_degraded.load(Ordering::Relaxed));
    }

    #[test]
    fn test_record_accept_then_close() {
        let m = ProxyMetrics::new();
        m.record_accept();
        m.record_accept();
        m.record_accept();
        assert_eq!(m.connections_total.load(Ordering::Relaxed), 3);
        assert_eq!(m.connections_active.load(Ordering::Relaxed), 3);
        m.record_close();
        assert_eq!(m.connections_total.load(Ordering::Relaxed), 3); // total never decreases
        assert_eq!(m.connections_active.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn test_healthcheck_flip_and_reset() {
        let m = ProxyMetrics::new();
        assert_eq!(m.record_healthcheck_fail(), 1);
        assert_eq!(m.record_healthcheck_fail(), 2);
        assert_eq!(m.record_healthcheck_fail(), 3);
        assert_eq!(m.backend_consecutive_failures.load(Ordering::Relaxed), 3);
        m.mark_degraded();
        assert!(m.backend_degraded.load(Ordering::Relaxed));

        // A successful check clears the failure streak and the degraded flag
        m.record_healthcheck_ok();
        assert_eq!(m.backend_consecutive_failures.load(Ordering::Relaxed), 0);
        assert!(!m.backend_degraded.load(Ordering::Relaxed));
        assert_eq!(m.backend_healthchecks_ok_total.load(Ordering::Relaxed), 1);
        assert_eq!(m.backend_healthchecks_fail_total.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn test_render_prometheus_format() {
        let m = ProxyMetrics::new();
        m.record_accept();
        m.record_accept();
        m.record_query();
        m.record_bytes_in(512);
        m.record_bytes_out(1024);
        m.record_reject_per_ip();

        let text = m.render(2, 5);

        // Prometheus text format requires # HELP and # TYPE preambles
        assert!(text.contains("# HELP pg_retest_proxy_connections_total"));
        assert!(text.contains("# TYPE pg_retest_proxy_connections_total counter"));
        assert!(text.contains("pg_retest_proxy_connections_total 2"));
        assert!(text.contains("pg_retest_proxy_connections_active 2"));
        assert!(text.contains("pg_retest_proxy_queries_total 1"));
        assert!(text.contains("pg_retest_proxy_bytes_in_total 512"));
        assert!(text.contains("pg_retest_proxy_bytes_out_total 1024"));
        assert!(text.contains("pg_retest_proxy_connections_rejected_total{reason=\"per_ip\"} 1"));
        assert!(text.contains("pg_retest_proxy_connections_rejected_total{reason=\"msg_size\"} 0"));
        assert!(text.contains("pg_retest_proxy_pool_active 2"));
        assert!(text.contains("pg_retest_proxy_pool_idle 5"));
        assert!(text.contains("pg_retest_proxy_backend_degraded 0"));
        assert!(text.contains("pg_retest_proxy_uptime_seconds"));
    }

    #[test]
    fn test_render_reflects_degraded_flag() {
        let m = ProxyMetrics::new();
        m.mark_degraded();
        let text = m.render(0, 0);
        assert!(text.contains("pg_retest_proxy_backend_degraded 1"));
    }

    #[test]
    fn test_record_error() {
        let m = ProxyMetrics::new();
        m.record_error();
        m.record_error();
        assert_eq!(m.errors_total.load(Ordering::Relaxed), 2);
    }
}
