use std::collections::VecDeque;

use anyhow::{bail, Result};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, Notify};
use tokio::time::{timeout, Duration};
use tracing::debug;

use super::socket;

/// A pooled server connection.
pub struct ServerConn {
    pub stream: TcpStream,
    pub id: u64,
}

/// Session-mode connection pool.
/// Each client gets a dedicated server connection for the entire session.
pub struct SessionPool {
    target: String,
    max_size: usize,
    pool_timeout: Duration,
    connect_timeout_secs: u64,
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
        Self {
            target,
            max_size,
            pool_timeout: Duration::from_secs(pool_timeout_secs),
            connect_timeout_secs,
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

                // Try to grab an idle connection
                if let Some(conn) = inner.idle.pop_front() {
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
                    return Ok(ServerConn { stream, id });
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
    pub async fn checkin(&self, conn: ServerConn) {
        let id = conn.id;
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
}
