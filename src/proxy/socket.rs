use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{Context, Result};
use socket2::{SockRef, TcpKeepalive};
use tokio::net::{TcpListener, TcpStream};
use tracing::debug;

/// Apply production-grade socket options to a connected TCP stream.
///
/// Sets TCP_NODELAY (disable Nagle) and TCP keepalive with:
/// - keepalive time: 60s (idle before first probe)
/// - keepalive interval: 10s (between probes)
/// - keepalive retries: 6 (before declaring dead)
pub fn configure_socket(stream: &TcpStream) -> Result<()> {
    stream
        .set_nodelay(true)
        .context("failed to set TCP_NODELAY")?;

    let sock_ref = SockRef::from(stream);

    let keepalive = TcpKeepalive::new()
        .with_time(Duration::from_secs(60))
        .with_interval(Duration::from_secs(10))
        .with_retries(6);

    sock_ref
        .set_tcp_keepalive(&keepalive)
        .context("failed to set TCP keepalive")?;

    debug!("Socket options applied: nodelay=true, keepalive=60s/10s/6");
    Ok(())
}

/// Create a TCP listener with a configurable listen backlog using socket2.
///
/// This gives control over the backlog parameter (default OS value is often 128,
/// which can cause connection drops under load).
pub async fn create_listener(addr: &str, backlog: u32) -> Result<TcpListener> {
    let socket_addr: SocketAddr = addr
        .parse()
        .context(format!("invalid listen address: {addr}"))?;

    let domain = if socket_addr.is_ipv6() {
        socket2::Domain::IPV6
    } else {
        socket2::Domain::IPV4
    };

    let socket = socket2::Socket::new(domain, socket2::Type::STREAM, Some(socket2::Protocol::TCP))
        .context("failed to create socket")?;

    // Allow address reuse to avoid EADDRINUSE on quick restarts
    socket
        .set_reuse_address(true)
        .context("failed to set SO_REUSEADDR")?;

    socket
        .set_nonblocking(true)
        .context("failed to set non-blocking")?;

    socket
        .bind(&socket_addr.into())
        .context(format!("failed to bind to {addr}"))?;

    socket
        .listen(backlog as i32)
        .context(format!("failed to listen with backlog {backlog}"))?;

    let std_listener: std::net::TcpListener = socket.into();
    let listener =
        TcpListener::from_std(std_listener).context("failed to convert to tokio listener")?;

    debug!("TCP listener created on {addr} with backlog={backlog}");
    Ok(listener)
}

/// Connect to a target address with a timeout.
///
/// Wraps `TcpStream::connect()` in `tokio::time::timeout()` to prevent
/// indefinite hangs when the target is unreachable.
pub async fn connect_with_timeout(target: &str, timeout_secs: u64) -> Result<TcpStream> {
    let duration = Duration::from_secs(timeout_secs);
    let stream = tokio::time::timeout(duration, TcpStream::connect(target))
        .await
        .context(format!(
            "connect to {target} timed out after {timeout_secs}s"
        ))?
        .context(format!("failed to connect to {target}"))?;

    configure_socket(&stream)?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener as TokioListener;

    #[tokio::test]
    async fn test_create_listener_with_backlog() {
        let listener = create_listener("127.0.0.1:0", 1024).await.unwrap();
        let addr = listener.local_addr().unwrap();
        assert!(addr.port() > 0);
    }

    #[tokio::test]
    async fn test_create_listener_ipv4() {
        let listener = create_listener("127.0.0.1:0", 128).await.unwrap();
        assert!(listener.local_addr().unwrap().is_ipv4());
    }

    #[tokio::test]
    async fn test_create_listener_invalid_addr() {
        let result = create_listener("not-an-address", 128).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid listen address"));
    }

    #[tokio::test]
    async fn test_configure_socket_sets_nodelay() {
        // Create a connected pair to test socket options
        let listener = TokioListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let connect = tokio::spawn(async move { TcpStream::connect(addr).await.unwrap() });

        let (server, _) = listener.accept().await.unwrap();
        let client = connect.await.unwrap();

        // Apply options to both sides
        configure_socket(&client).unwrap();
        configure_socket(&server).unwrap();

        assert!(client.nodelay().unwrap());
        assert!(server.nodelay().unwrap());
    }

    #[tokio::test]
    async fn test_connect_with_timeout_success() {
        let listener = TokioListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let connect = tokio::spawn(async move { connect_with_timeout(&addr.to_string(), 5).await });

        let (_server, _) = listener.accept().await.unwrap();
        let client = connect.await.unwrap().unwrap();

        // Verify socket options were applied
        assert!(client.nodelay().unwrap());
    }

    #[tokio::test]
    async fn test_connect_with_timeout_expires() {
        // Use a non-routable address to trigger a timeout
        // 192.0.2.1 is TEST-NET-1 (RFC 5737), typically not routable
        let result = connect_with_timeout("192.0.2.1:9999", 1).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("timed out") || err.contains("connect"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_connect_with_timeout_refused() {
        // Connect to a port that is definitely not listening
        let result = connect_with_timeout("127.0.0.1:1", 2).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_create_listener_backlog_values() {
        // Test that different backlog values work
        for backlog in [1, 128, 1024, 4096] {
            let listener = create_listener("127.0.0.1:0", backlog).await.unwrap();
            assert!(listener.local_addr().unwrap().port() > 0);
        }
    }
}
