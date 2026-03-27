use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Result};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, Notify};
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use super::socket;

/// A pooled server connection with lifecycle metadata.
pub struct ServerConn {
    pub stream: TcpStream,
    pub id: u64,
    /// When this connection was first created.
    pub created_at: Instant,
    /// When this connection was last returned to the idle queue.
    /// `None` while the connection is checked out (active).
    pub idle_since: Option<Instant>,
}

/// Session-mode connection pool.
/// Each client gets a dedicated server connection for the entire session.
pub struct SessionPool {
    target: String,
    max_size: usize,
    pool_timeout: Duration,
    connect_timeout_secs: u64,
    /// Maximum lifetime of a connection in seconds. Connections older than this
    /// are discarded on checkin instead of being returned to the idle queue.
    /// 0 means unlimited.
    server_lifetime_secs: u64,
    /// Maximum idle time in seconds. Connections idle longer than this are
    /// reaped by the background idle reaper. 0 means no reaping.
    server_idle_timeout_secs: u64,
    inner: Mutex<PoolInner>,
    notify: Notify,
}

struct PoolInner {
    idle: VecDeque<ServerConn>,
    active_count: usize,
    next_id: u64,
}

impl SessionPool {
    pub fn new(
        target: String,
        max_size: usize,
        pool_timeout_secs: u64,
        connect_timeout_secs: u64,
    ) -> Self {
        Self::with_lifecycle(
            target,
            max_size,
            pool_timeout_secs,
            connect_timeout_secs,
            0,
            0,
        )
    }

    /// Create a pool with full lifecycle configuration.
    pub fn with_lifecycle(
        target: String,
        max_size: usize,
        pool_timeout_secs: u64,
        connect_timeout_secs: u64,
        server_lifetime_secs: u64,
        server_idle_timeout_secs: u64,
    ) -> Self {
        Self {
            target,
            max_size,
            pool_timeout: Duration::from_secs(pool_timeout_secs),
            connect_timeout_secs,
            server_lifetime_secs,
            server_idle_timeout_secs,
            inner: Mutex::new(PoolInner {
                idle: VecDeque::new(),
                active_count: 0,
                next_id: 1,
            }),
            notify: Notify::new(),
        }
    }

    /// Checkout a server connection from the pool.
    /// Returns an idle connection or opens a new one if under the limit.
    /// Waits up to pool_timeout if at capacity.
    pub async fn checkout(&self) -> Result<ServerConn> {
        let deadline = tokio::time::Instant::now() + self.pool_timeout;

        loop {
            {
                let mut inner = self.inner.lock().await;

                // Try to grab an idle connection, skipping expired ones
                while let Some(mut conn) = inner.idle.pop_front() {
                    // Skip connections that exceeded their lifetime
                    if self.server_lifetime_secs > 0
                        && conn.created_at.elapsed().as_secs() >= self.server_lifetime_secs
                    {
                        debug!(
                            "Pool: discarding conn {} on checkout (lifetime {:.0}s exceeded {}s)",
                            conn.id,
                            conn.created_at.elapsed().as_secs_f64(),
                            self.server_lifetime_secs
                        );
                        // Connection is dropped here, closing the socket
                        continue;
                    }

                    // Skip connections that have been idle too long
                    if self.server_idle_timeout_secs > 0 {
                        if let Some(idle_since) = conn.idle_since {
                            if idle_since.elapsed().as_secs() >= self.server_idle_timeout_secs {
                                debug!(
                                    "Pool: discarding conn {} on checkout (idle {:.0}s exceeded {}s)",
                                    conn.id,
                                    idle_since.elapsed().as_secs_f64(),
                                    self.server_idle_timeout_secs
                                );
                                continue;
                            }
                        }
                    }

                    conn.idle_since = None; // Mark as active
                    inner.active_count += 1;
                    debug!(
                        "Pool: checkout idle conn {} (active={}, idle={})",
                        conn.id,
                        inner.active_count,
                        inner.idle.len()
                    );
                    return Ok(conn);
                }

                // Try to open a new connection
                let total = inner.active_count + inner.idle.len();
                if total < self.max_size {
                    let id = inner.next_id;
                    inner.next_id += 1;
                    inner.active_count += 1;
                    debug!(
                        "Pool: opening new conn {id} to {} (active={}, idle={})",
                        self.target,
                        inner.active_count,
                        inner.idle.len()
                    );
                    drop(inner); // Release lock before connecting

                    let stream =
                        socket::connect_with_timeout(&self.target, self.connect_timeout_secs)
                            .await?;
                    return Ok(ServerConn {
                        stream,
                        id,
                        created_at: Instant::now(),
                        idle_since: None,
                    });
                }
            }

            // At capacity — wait for a connection to be returned
            let remaining = deadline - tokio::time::Instant::now();
            if remaining.is_zero() {
                bail!(
                    "Connection pool exhausted (max_size={}). Timed out waiting for a connection.",
                    self.max_size
                );
            }

            match timeout(remaining, self.notify.notified()).await {
                Ok(()) => continue, // A connection was returned, try again
                Err(_) => bail!(
                    "Connection pool exhausted (max_size={}). Timed out waiting for a connection.",
                    self.max_size
                ),
            }
        }
    }

    /// Return a server connection to the pool.
    /// If the connection has exceeded its maximum lifetime, it is discarded
    /// instead of being returned to the idle queue.
    pub async fn checkin(&self, mut conn: ServerConn) {
        let id = conn.id;

        // Check if the connection has exceeded its maximum lifetime
        if self.server_lifetime_secs > 0
            && conn.created_at.elapsed().as_secs() >= self.server_lifetime_secs
        {
            debug!(
                "Pool: recycling conn {id} (lifetime {:.0}s exceeded {}s)",
                conn.created_at.elapsed().as_secs_f64(),
                self.server_lifetime_secs
            );
            // Just discard — decrement active_count and drop the connection
            let mut inner = self.inner.lock().await;
            inner.active_count = inner.active_count.saturating_sub(1);
            drop(inner);
            self.notify.notify_one();
            return;
        }

        conn.idle_since = Some(Instant::now());
        let mut inner = self.inner.lock().await;
        inner.active_count = inner.active_count.saturating_sub(1);
        inner.idle.push_back(conn);
        debug!(
            "Pool: checkin conn {id} (active={}, idle={})",
            inner.active_count,
            inner.idle.len()
        );
        drop(inner);
        self.notify.notify_one();
    }

    /// Discard a server connection (don't return to pool).
    pub async fn discard(&self) {
        let mut inner = self.inner.lock().await;
        inner.active_count = inner.active_count.saturating_sub(1);
        drop(inner);
        self.notify.notify_one();
    }

    /// Get current pool stats (active_count, idle_count).
    pub async fn stats(&self) -> (usize, usize) {
        let inner = self.inner.lock().await;
        (inner.active_count, inner.idle.len())
    }

    /// Reap idle connections that have been idle longer than `server_idle_timeout_secs`.
    /// Returns the number of connections reaped.
    pub async fn reap_idle(&self) -> usize {
        if self.server_idle_timeout_secs == 0 {
            return 0;
        }

        let mut inner = self.inner.lock().await;
        let before = inner.idle.len();

        inner.idle.retain(|conn| {
            if let Some(idle_since) = conn.idle_since {
                idle_since.elapsed().as_secs() < self.server_idle_timeout_secs
            } else {
                true // no idle_since means it shouldn't be in the idle queue, but keep it
            }
        });

        let reaped = before - inner.idle.len();
        if reaped > 0 {
            debug!(
                "Pool: reaped {} idle connections (remaining idle={})",
                reaped,
                inner.idle.len()
            );
        }
        reaped
    }

    /// Spawn a background idle reaper task that periodically removes stale idle connections.
    /// The reaper runs at half the idle timeout interval (minimum 10s).
    /// Returns a JoinHandle for the reaper task.
    pub fn spawn_idle_reaper(
        self: &Arc<Self>,
        cancel: CancellationToken,
    ) -> Option<tokio::task::JoinHandle<()>> {
        if self.server_idle_timeout_secs == 0 {
            return None;
        }

        let interval_secs = (self.server_idle_timeout_secs / 2).max(10);
        let pool = Arc::clone(self);

        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
            interval.tick().await; // first tick is immediate, skip it

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let reaped = pool.reap_idle().await;
                        if reaped > 0 {
                            info!("Idle reaper: closed {reaped} stale connection(s)");
                        }
                    }
                    _ = cancel.cancelled() => {
                        debug!("Idle reaper shutting down");
                        break;
                    }
                }
            }
        });

        Some(handle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// Helper: create a real TcpStream pair via loopback for test ServerConns.
    async fn make_test_stream() -> TcpStream {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connect_fut = TcpStream::connect(addr);
        let (stream, _) = tokio::join!(connect_fut, listener.accept());
        stream.unwrap()
    }

    /// Helper: create a ServerConn with a real stream and custom timestamps.
    async fn make_test_conn(
        id: u64,
        created_at: Instant,
        idle_since: Option<Instant>,
    ) -> ServerConn {
        ServerConn {
            stream: make_test_stream().await,
            id,
            created_at,
            idle_since,
        }
    }

    #[tokio::test]
    async fn test_server_conn_timestamps() {
        let now = Instant::now();
        let conn = make_test_conn(1, now, None).await;
        assert_eq!(conn.id, 1);
        assert!(conn.created_at.elapsed().as_millis() < 500);
        assert!(conn.idle_since.is_none());
    }

    #[tokio::test]
    async fn test_server_conn_idle_since() {
        let now = Instant::now();
        let conn = make_test_conn(2, now, Some(now)).await;
        assert!(conn.idle_since.is_some());
        assert!(conn.idle_since.unwrap().elapsed().as_millis() < 500);
    }

    #[tokio::test]
    async fn test_pool_with_lifecycle_defaults() {
        let pool = SessionPool::with_lifecycle("127.0.0.1:5432".to_string(), 10, 30, 5, 0, 0);
        assert_eq!(pool.server_lifetime_secs, 0);
        assert_eq!(pool.server_idle_timeout_secs, 0);
    }

    #[tokio::test]
    async fn test_reap_idle_noop_when_disabled() {
        let pool = SessionPool::new("127.0.0.1:5432".to_string(), 10, 30, 5);
        let reaped = pool.reap_idle().await;
        assert_eq!(reaped, 0);
    }

    #[tokio::test]
    async fn test_spawn_idle_reaper_none_when_disabled() {
        let pool = Arc::new(SessionPool::new("127.0.0.1:5432".to_string(), 10, 30, 5));
        let cancel = CancellationToken::new();
        let handle = pool.spawn_idle_reaper(cancel);
        assert!(handle.is_none());
    }

    #[tokio::test]
    async fn test_spawn_idle_reaper_some_when_enabled() {
        let pool = Arc::new(SessionPool::with_lifecycle(
            "127.0.0.1:5432".to_string(),
            10,
            30,
            5,
            0,
            60,
        ));
        let cancel = CancellationToken::new();
        let handle = pool.spawn_idle_reaper(cancel.clone());
        assert!(handle.is_some());
        cancel.cancel();
        handle.unwrap().await.unwrap();
    }

    #[tokio::test]
    async fn test_checkin_recycles_expired_connection() {
        let pool = SessionPool::with_lifecycle(
            "127.0.0.1:5432".to_string(),
            10,
            30,
            5,
            1, // 1 second lifetime
            0,
        );

        let conn = make_test_conn(42, Instant::now() - Duration::from_secs(2), None).await;

        {
            let mut inner = pool.inner.lock().await;
            inner.active_count = 1;
        }

        pool.checkin(conn).await;

        let (active, idle) = pool.stats().await;
        assert_eq!(active, 0);
        assert_eq!(idle, 0);
    }

    #[tokio::test]
    async fn test_checkin_returns_fresh_connection() {
        let pool = SessionPool::with_lifecycle("127.0.0.1:5432".to_string(), 10, 30, 5, 3600, 0);

        let conn = make_test_conn(43, Instant::now(), None).await;

        {
            let mut inner = pool.inner.lock().await;
            inner.active_count = 1;
        }

        pool.checkin(conn).await;

        let (active, idle) = pool.stats().await;
        assert_eq!(active, 0);
        assert_eq!(idle, 1);
    }

    #[tokio::test]
    async fn test_reap_idle_removes_stale_connections() {
        let pool = SessionPool::with_lifecycle(
            "127.0.0.1:5432".to_string(),
            10,
            30,
            5,
            0,
            1, // 1 second idle timeout
        );

        {
            let mut inner = pool.inner.lock().await;
            inner.idle.push_back(
                make_test_conn(
                    100,
                    Instant::now() - Duration::from_secs(10),
                    Some(Instant::now() - Duration::from_secs(2)),
                )
                .await,
            );
            inner
                .idle
                .push_back(make_test_conn(101, Instant::now(), Some(Instant::now())).await);
        }

        let reaped = pool.reap_idle().await;
        assert_eq!(reaped, 1);

        let (_, idle) = pool.stats().await;
        assert_eq!(idle, 1);
    }

    #[tokio::test]
    async fn test_checkout_skips_expired_lifetime_connections() {
        let pool = SessionPool::with_lifecycle(
            "127.0.0.1:5432".to_string(),
            10,
            30,
            5,
            1, // 1 second lifetime
            0,
        );

        {
            let mut inner = pool.inner.lock().await;
            inner.idle.push_back(
                make_test_conn(
                    200,
                    Instant::now() - Duration::from_secs(2),
                    Some(Instant::now()),
                )
                .await,
            );
        }

        // Checkout will skip the expired connection and try to open a new one.
        // That will fail (no real server), but the expired connection should be gone.
        let _result = pool.checkout().await;
        let inner = pool.inner.lock().await;
        assert!(inner.idle.is_empty());
    }

    #[tokio::test]
    async fn test_checkout_skips_idle_timeout_connections() {
        let pool = SessionPool::with_lifecycle(
            "127.0.0.1:5432".to_string(),
            10,
            30,
            5,
            0,
            1, // 1 second idle timeout
        );

        {
            let mut inner = pool.inner.lock().await;
            inner.idle.push_back(
                make_test_conn(
                    300,
                    Instant::now(),
                    Some(Instant::now() - Duration::from_secs(2)),
                )
                .await,
            );
        }

        let _result = pool.checkout().await;
        let inner = pool.inner.lock().await;
        assert!(inner.idle.is_empty());
    }
}
