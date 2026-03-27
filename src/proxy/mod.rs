pub mod capture;
pub mod connection;
pub mod control;
pub mod listener;
pub mod pool;
pub mod protocol;
pub mod socket;
pub mod staging;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use self::capture::{
    build_profile, build_profile_from_staging, run_collector, run_staging_collector, CaptureEvent,
};
use self::connection::{ImplicitCaptureState, TimeoutConfig};
use self::control::{build_control_router, CaptureCommand, ControlState};
use self::pool::SessionPool;
use self::staging::StagingDb;
use crate::correlate::sequence::SequenceState;
use crate::profile::io;
use crate::profile::WorkloadProfile;

/// Configuration for the proxy server.
pub struct ProxyConfig {
    pub listen_addr: String,
    pub target_addr: String,
    pub output: Option<PathBuf>,
    pub pool_size: usize,
    pub pool_timeout_secs: u64,
    pub mask_values: bool,
    pub no_capture: bool,
    pub duration: Option<std::time::Duration>,
    pub persistent: bool,
    pub control_port: Option<u16>,
    /// Max queries before auto-stopping capture (0 = unlimited)
    pub max_capture_queries: u64,
    /// Max staging DB size in bytes before auto-stopping capture (0 = unlimited)
    pub max_capture_bytes: u64,
    /// Max capture duration before auto-stopping (None = unlimited)
    pub max_capture_duration: Option<std::time::Duration>,
    /// Pre-captured sequence snapshot to embed in the profile metadata.
    pub sequence_snapshot: Option<Vec<SequenceState>>,
    /// Enable RETURNING clause correlation capture (id_mode correlate or full).
    pub enable_correlation: bool,
    /// Auto-inject RETURNING for bare INSERTs and intercept currval/lastval.
    pub id_capture_implicit: bool,
    /// Primary key map for implicit RETURNING injection (discovered at startup).
    pub pk_map: Option<Vec<crate::correlate::capture::TablePk>>,
    /// Disable stealth mode: forward auto-injected RETURNING results to client.
    /// When false (default), injected RETURNING results are suppressed.
    pub no_stealth: bool,
    /// Optional shared AtomicBool for the managed proxy's no_capture flag.
    /// When provided, the web toggle handler can read/verify capture state directly.
    pub shared_no_capture: Option<Arc<AtomicBool>>,
    /// TCP listen backlog size (default 1024).
    pub listen_backlog: u32,
    /// Connect timeout in seconds when opening connections to the target (default 5).
    pub connect_timeout_secs: u64,
    /// Client idle timeout in seconds (default 300). 0 = no timeout.
    pub client_timeout_secs: u64,
    /// Server idle timeout in seconds (default 300). 0 = no timeout.
    pub server_timeout_secs: u64,
    /// Auth/login timeout in seconds (default 30). 0 = no timeout.
    pub auth_timeout_secs: u64,
    /// Maximum lifetime of a server connection in seconds (default 3600).
    /// Connections older than this are discarded on checkin. 0 = unlimited.
    pub server_lifetime_secs: u64,
    /// Maximum idle time for pooled connections in seconds (default 600).
    /// A background reaper closes connections idle longer than this. 0 = no reaping.
    pub server_idle_timeout_secs: u64,
    /// Idle-in-transaction warning threshold in seconds (default 0 = disabled).
    /// Logs a warning when a connection appears idle-in-transaction beyond this threshold.
    /// Does NOT forcibly close the connection.
    pub idle_transaction_timeout_secs: u64,
    /// Maximum PG protocol message size in bytes (default 67_108_864 = 64MB).
    /// Messages exceeding this limit are rejected with a PG ErrorResponse.
    /// 0 = unlimited.
    pub max_message_size: u32,
    /// Maximum concurrent connections from a single source IP (default 0 = unlimited).
    /// When exceeded, new connections from that IP are rejected with a PG ErrorResponse.
    pub max_connections_per_ip: u32,
    /// Shutdown timeout in seconds — how long to wait for active connections to drain
    /// before forcing close (default 30). 0 = force close immediately.
    pub shutdown_timeout_secs: u64,
    /// Optional TLS acceptor for client-facing connections.
    /// When set, the proxy accepts SSLRequest from clients and upgrades connections to TLS.
    pub client_tls_acceptor: Option<tokio_rustls::TlsAcceptor>,
}

/// Build an `ImplicitCaptureState` from the proxy config if implicit capture is enabled.
fn build_implicit_capture_state(config: &ProxyConfig) -> Option<Arc<ImplicitCaptureState>> {
    if config.id_capture_implicit {
        let pk_map = config.pk_map.clone().unwrap_or_default();
        Some(Arc::new(ImplicitCaptureState {
            pk_map,
            stealth: !config.no_stealth,
        }))
    } else {
        None
    }
}

/// Build a `TimeoutConfig` from the proxy config.
fn build_timeout_config(config: &ProxyConfig) -> TimeoutConfig {
    TimeoutConfig::from_secs_full(
        config.client_timeout_secs,
        config.server_timeout_secs,
        config.auth_timeout_secs,
        config.idle_transaction_timeout_secs,
    )
    .with_max_message_size(config.max_message_size)
}

/// Wait for active connections to drain, polling every 250ms.
/// Returns immediately when active_count reaches 0.
/// If the timeout expires, logs a warning and returns.
async fn drain_connections(pool: &SessionPool, timeout: std::time::Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let (active, _idle) = pool.stats().await;
        if active == 0 {
            info!("All connections drained");
            break;
        }
        if Instant::now() >= deadline {
            warn!(
                "Shutdown timeout: {} connection(s) still active, forcing close",
                active
            );
            break;
        }
        info!("Draining connections... {} active", active);
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// Run the proxy server (CLI mode — signal-based shutdown).
pub async fn run_proxy(config: ProxyConfig) -> Result<()> {
    if config.persistent {
        return run_proxy_persistent(config).await;
    }

    let listener = socket::create_listener(&config.listen_addr, config.listen_backlog).await?;
    let pool = Arc::new(SessionPool::with_lifecycle(
        config.target_addr.clone(),
        config.pool_size,
        config.pool_timeout_secs,
        config.connect_timeout_secs,
        config.server_lifetime_secs,
        config.server_idle_timeout_secs,
    ));

    // Spawn idle reaper if configured
    let _reaper_handle = pool.spawn_idle_reaper(CancellationToken::new());

    let (capture_tx, capture_rx) = mpsc::unbounded_channel();

    // Spawn capture collector
    let collector_handle = tokio::spawn(async move { run_collector(capture_rx).await });

    // Spawn listener (no metrics channel in CLI mode)
    let pool_clone = pool.clone();
    let no_capture = Arc::new(AtomicBool::new(config.no_capture));
    let enable_correlation = config.enable_correlation;
    let max_conns_per_ip = config.max_connections_per_ip;
    let implicit_capture = build_implicit_capture_state(&config);
    let timeouts = build_timeout_config(&config);
    let tls_acceptor = config.client_tls_acceptor.map(Arc::new);
    let listener_handle = tokio::spawn(async move {
        listener::run_listener(
            listener,
            pool_clone,
            capture_tx,
            no_capture,
            None,
            enable_correlation,
            implicit_capture,
            timeouts,
            max_conns_per_ip,
            tls_acceptor,
        )
        .await
    });

    // Wait for shutdown signal or duration (signals are handled in both cases
    // so that SIGTERM/SIGINT during a --duration wait still flushes the workload).
    match config.duration {
        Some(dur) => {
            info!("Proxy will run for {:?}", dur);
            tokio::select! {
                _ = tokio::time::sleep(dur) => {
                    info!("Duration elapsed, shutting down...");
                }
                _ = async {
                    #[cfg(unix)]
                    {
                        use tokio::signal::unix::{signal, SignalKind};
                        let mut sigterm = signal(SignalKind::terminate()).unwrap();
                        tokio::select! {
                            _ = tokio::signal::ctrl_c() => {},
                            _ = sigterm.recv() => {},
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        tokio::signal::ctrl_c().await.unwrap();
                    }
                } => {
                    info!("Shutdown signal received...");
                }
            }
        }
        None => {
            info!("Press Ctrl+C to stop and save captured workload");
            #[cfg(unix)]
            {
                use tokio::signal::unix::{signal, SignalKind};
                let mut sigterm = signal(SignalKind::terminate())?;
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {},
                    _ = sigterm.recv() => {},
                }
            }
            #[cfg(not(unix))]
            {
                tokio::signal::ctrl_c().await?;
            }
            info!("Shutdown signal received...");
        }
    }

    // Abort the listener to stop accepting new connections
    listener_handle.abort();

    // Wait for active connections to drain
    drain_connections(
        &pool,
        std::time::Duration::from_secs(config.shutdown_timeout_secs),
    )
    .await;

    // Wait for collector to finish (channel senders will be dropped)
    let captured = collector_handle.await?;

    if config.no_capture {
        info!("Proxy stopped (capture was disabled)");
        return Ok(());
    }

    // Build and write profile
    let source_host = config.target_addr.clone();
    let mut profile = build_profile(captured, &source_host, config.mask_values);

    // Inject sequence snapshot if captured
    if config.sequence_snapshot.is_some() {
        profile.metadata.sequence_snapshot = config.sequence_snapshot;
    }

    info!(
        "Captured {} queries across {} sessions",
        profile.metadata.total_queries, profile.metadata.total_sessions
    );

    if let Some(ref output) = config.output {
        io::write_profile(output, &profile)?;
        info!("Wrote workload profile to {}", output.display());
    } else {
        info!("No output path specified — skipping profile write");
    }

    Ok(())
}

/// Run the proxy in persistent mode (stays running, capture controlled via HTTP).
async fn run_proxy_persistent(config: ProxyConfig) -> Result<()> {
    let listener = socket::create_listener(&config.listen_addr, config.listen_backlog).await?;
    let pool = Arc::new(SessionPool::with_lifecycle(
        config.target_addr.clone(),
        config.pool_size,
        config.pool_timeout_secs,
        config.connect_timeout_secs,
        config.server_lifetime_secs,
        config.server_idle_timeout_secs,
    ));

    // Spawn idle reaper if configured
    let _reaper_handle = pool.spawn_idle_reaper(CancellationToken::new());

    // Shared no_capture flag — starts as true (no capture until start-capture is called)
    let no_capture = Arc::new(AtomicBool::new(true));

    // Channel for capture commands from the control endpoint
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<CaptureCommand>();

    // Set up control state
    let control_port = config.control_port.unwrap_or(6544);
    let control_state = Arc::new(RwLock::new(ControlState {
        running: true,
        capturing: false,
        capture_id: None,
        active_sessions: 0,
        total_queries: 0,
        started_at: Instant::now(),
        capture_cmd_tx: Some(cmd_tx),
        staging_db: None, // Set after staging_db is opened below
    }));

    // Start the control HTTP endpoint
    let control_router = build_control_router(control_state.clone());
    let control_addr = format!("0.0.0.0:{control_port}");
    let control_listener = TcpListener::bind(&control_addr).await?;
    info!("Persistent proxy control endpoint on http://{control_addr}");

    tokio::spawn(async move {
        if let Err(e) = axum::serve(control_listener, control_router).await {
            tracing::error!("Control endpoint error: {e}");
        }
    });

    // Open standalone staging DB
    let staging_db = StagingDb::open_standalone(&PathBuf::from("proxy-capture.db")).await?;

    // Store staging_db in control state for recover/discard endpoints
    {
        let mut cs = control_state.write().await;
        cs.staging_db = Some(staging_db.clone());
    }

    // Spawn the listener — it runs for the entire lifetime of the proxy.
    // We need a capture_tx that stays alive. We'll swap the receiver on start/stop.
    let (persistent_capture_tx, persistent_capture_rx) = mpsc::unbounded_channel::<CaptureEvent>();

    let pool_clone = pool.clone();
    let no_capture_clone = no_capture.clone();
    let enable_correlation = config.enable_correlation;
    let max_conns_per_ip_persistent = config.max_connections_per_ip;
    let implicit_capture = build_implicit_capture_state(&config);
    let timeouts = build_timeout_config(&config);
    let tls_acceptor_persistent = config.client_tls_acceptor.map(Arc::new);
    let listener_handle = tokio::spawn(async move {
        listener::run_listener(
            listener,
            pool_clone,
            persistent_capture_tx,
            no_capture_clone,
            None,
            enable_correlation,
            implicit_capture,
            timeouts,
            max_conns_per_ip_persistent,
            tls_acceptor_persistent,
        )
        .await
    });

    // We need to forward events from the persistent channel to the active staging collector.
    // Use an intermediary: a shared optional sender that the forwarder writes to.
    let active_staging_tx: Arc<tokio::sync::Mutex<Option<mpsc::UnboundedSender<CaptureEvent>>>> =
        Arc::new(tokio::sync::Mutex::new(None));

    // Capture safeguard limits
    let max_queries = config.max_capture_queries;
    let max_bytes = config.max_capture_bytes;
    let max_duration = config.max_capture_duration;

    // Auto-stop channel: forwarder sends here when limits are exceeded
    let (auto_stop_tx, auto_stop_rx) = mpsc::unbounded_channel::<String>(); // reason string

    // Spawn a forwarder task that reads from persistent_capture_rx, updates
    // ControlState counters, checks safeguard limits, and forwards to the active staging collector.
    let active_staging_tx_clone = active_staging_tx.clone();
    let control_state_fwd = control_state.clone();
    let no_capture_fwd = no_capture.clone();
    let staging_db_fwd = staging_db.clone();
    let forwarder_handle = tokio::spawn(async move {
        let mut rx = persistent_capture_rx;
        let mut capture_query_count: u64 = 0;
        let mut capture_start: Option<Instant> = None;
        let mut limit_triggered = false;
        let mut last_size_check: u64 = 0; // check DB size every 1000 queries

        while let Some(event) = rx.recv().await {
            // Update ControlState counters for status endpoint
            match &event {
                CaptureEvent::SessionStart { .. } => {
                    let mut cs = control_state_fwd.write().await;
                    cs.active_sessions += 1;
                    if capture_start.is_none() && cs.capturing {
                        capture_start = Some(Instant::now());
                    }
                }
                CaptureEvent::SessionEnd { .. } => {
                    let mut cs = control_state_fwd.write().await;
                    cs.active_sessions = cs.active_sessions.saturating_sub(1);
                }
                CaptureEvent::QueryComplete { .. } | CaptureEvent::QueryError { .. } => {
                    let mut cs = control_state_fwd.write().await;
                    cs.total_queries += 1;
                    capture_query_count += 1;

                    // Check query count limit
                    if !limit_triggered && max_queries > 0 && capture_query_count >= max_queries {
                        limit_triggered = true;
                        warn!("Capture safeguard: max query count ({}) reached, auto-stopping capture", max_queries);
                        no_capture_fwd.store(true, Ordering::Relaxed);
                        let _ = auto_stop_tx.send(format!("max_queries_reached ({})", max_queries));
                    }

                    // Check duration limit
                    if !limit_triggered {
                        if let (Some(max_dur), Some(start)) = (max_duration, capture_start) {
                            if start.elapsed() >= max_dur {
                                limit_triggered = true;
                                warn!("Capture safeguard: max duration ({:?}) reached, auto-stopping capture", max_dur);
                                no_capture_fwd.store(true, Ordering::Relaxed);
                                let _ = auto_stop_tx
                                    .send(format!("max_duration_reached ({:?})", max_dur));
                            }
                        }
                    }

                    // Check staging DB size limit (every 1000 queries to avoid overhead)
                    if !limit_triggered
                        && max_bytes > 0
                        && capture_query_count - last_size_check >= 1000
                    {
                        last_size_check = capture_query_count;
                        if let Ok(size) = staging_db_fwd.db_size_bytes().await {
                            if size >= max_bytes {
                                limit_triggered = true;
                                warn!("Capture safeguard: staging DB size ({} bytes) exceeds limit ({} bytes), auto-stopping capture", size, max_bytes);
                                no_capture_fwd.store(true, Ordering::Relaxed);
                                let _ =
                                    auto_stop_tx.send(format!("max_size_reached ({} bytes)", size));
                            }
                        }
                    }
                }
                _ => {}
            }

            // Forward to active staging collector (if any)
            let guard = active_staging_tx_clone.lock().await;
            if let Some(ref tx) = *guard {
                let _ = tx.send(event);
            }
        }
    });

    // Track the active collector task
    let mut active_collector: Option<tokio::task::JoinHandle<()>> = None;
    let mut active_capture_id: Option<String> = None;

    info!(
        "Persistent proxy running on {} (target: {})",
        config.listen_addr, config.target_addr
    );
    info!("Use POST http://localhost:{control_port}/start-capture to begin capturing");

    // Wrap auto_stop_rx for use in select
    let mut auto_stop_rx = auto_stop_rx;

    // Main command loop — also listen for shutdown signals and auto-stop
    loop {
        let cmd = tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(c) => c,
                    None => break, // Command channel closed
                }
            }
            reason = auto_stop_rx.recv() => {
                // Safeguard triggered — synthesize a Stop command
                if let Some(reason) = reason {
                    warn!("Auto-stopping capture: {reason}");
                    // Create a dummy reply channel (we log the result ourselves)
                    let (reply_tx, _reply_rx) = tokio::sync::oneshot::channel();
                    CaptureCommand::Stop { output: None, reply: reply_tx }
                } else {
                    continue;
                }
            }
            _ = async {
                #[cfg(unix)]
                {
                    use tokio::signal::unix::{signal, SignalKind};
                    let mut sigterm = signal(SignalKind::terminate()).unwrap();
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => {},
                        _ = sigterm.recv() => {},
                    }
                }
                #[cfg(not(unix))]
                {
                    tokio::signal::ctrl_c().await.unwrap();
                }
            } => {
                info!("Shutdown signal received, stopping persistent proxy...");
                break;
            }
        };

        match cmd {
            CaptureCommand::Start { reply } => {
                if active_capture_id.is_some() {
                    let _ = reply.send(Err("Already capturing".to_string()));
                    continue;
                }

                let capture_id = Uuid::new_v4().to_string();
                info!("Starting capture session: {capture_id}");

                // Create a new staging collector channel
                let (staging_tx, staging_rx) = mpsc::unbounded_channel::<CaptureEvent>();

                // Set the forwarder to send to this new staging collector
                {
                    let mut guard = active_staging_tx.lock().await;
                    *guard = Some(staging_tx);
                }

                // Spawn staging collector
                let db_clone = staging_db.clone();
                let cap_id_clone = capture_id.clone();
                active_collector = Some(tokio::spawn(async move {
                    run_staging_collector(staging_rx, db_clone, cap_id_clone).await;
                }));

                // Enable capture
                no_capture.store(false, Ordering::Relaxed);

                // Update control state
                {
                    let mut state = control_state.write().await;
                    state.capturing = true;
                    state.capture_id = Some(capture_id.clone());
                }

                active_capture_id = Some(capture_id.clone());
                let _ = reply.send(Ok(capture_id));
            }
            CaptureCommand::Stop { output, reply } => {
                let capture_id = match active_capture_id.take() {
                    Some(id) => id,
                    None => {
                        let _ = reply.send(Err("Not currently capturing".to_string()));
                        continue;
                    }
                };

                info!("Stopping capture session: {capture_id}");

                // Disable capture (stop emitting events)
                no_capture.store(true, Ordering::Relaxed);

                // Close the staging collector channel by clearing the forwarder target
                {
                    let mut guard = active_staging_tx.lock().await;
                    *guard = None; // Drops the sender, closing the staging collector's channel
                }

                // Wait for the collector to finish processing
                if let Some(handle) = active_collector.take() {
                    let _ = handle.await;
                }

                // Read staged data and build profile
                let rows = match staging_db.read_capture(&capture_id).await {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = reply.send(Err(format!("Failed to read staging data: {e}")));
                        continue;
                    }
                };

                let profile =
                    build_profile_from_staging(rows, &config.target_addr, config.mask_values);

                info!(
                    "Captured {} queries across {} sessions",
                    profile.metadata.total_queries, profile.metadata.total_sessions
                );

                // Determine output path
                let output_path = output.or_else(|| config.output.clone()).unwrap_or_else(|| {
                    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                    PathBuf::from(format!("capture-{ts}.wkl"))
                });

                // Write profile
                if let Err(e) = io::write_profile(&output_path, &profile) {
                    let _ = reply.send(Err(format!("Failed to write profile: {e}")));
                    // Clean up staging data even on write failure
                    let _ = staging_db.clear_capture(&capture_id).await;
                    // Update control state
                    let mut state = control_state.write().await;
                    state.capturing = false;
                    state.capture_id = None;
                    continue;
                }

                info!("Wrote workload profile to {}", output_path.display());

                // Clear staging data
                let _ = staging_db.clear_capture(&capture_id).await;

                // Update control state
                {
                    let mut state = control_state.write().await;
                    state.capturing = false;
                    state.capture_id = None;
                }

                let _ = reply.send(Ok(serde_json::json!({
                    "ok": true,
                    "capture_id": capture_id,
                    "output": output_path.to_string_lossy(),
                    "total_sessions": profile.metadata.total_sessions,
                    "total_queries": profile.metadata.total_queries,
                })));
            }
        }
    }

    // Shutdown: if we were capturing, flush the workload to disk before exiting
    if let Some(capture_id) = active_capture_id.take() {
        info!("Flushing active capture {capture_id} during shutdown...");
        no_capture.store(true, Ordering::Relaxed);
        {
            let mut guard = active_staging_tx.lock().await;
            *guard = None;
        }
        if let Some(handle) = active_collector.take() {
            let _ = handle.await;
        }

        // Read staged data and build profile
        match staging_db.read_capture(&capture_id).await {
            Ok(rows) => {
                let profile =
                    build_profile_from_staging(rows, &config.target_addr, config.mask_values);
                info!(
                    "Captured {} queries across {} sessions",
                    profile.metadata.total_queries, profile.metadata.total_sessions
                );

                let output_path = config.output.clone().unwrap_or_else(|| {
                    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                    PathBuf::from(format!("capture-{ts}.wkl"))
                });

                match io::write_profile(&output_path, &profile) {
                    Ok(()) => info!("Wrote workload profile to {}", output_path.display()),
                    Err(e) => warn!("Failed to write profile during shutdown: {e}"),
                }

                let _ = staging_db.clear_capture(&capture_id).await;
            }
            Err(e) => {
                warn!("Failed to read staging data during shutdown: {e}");
            }
        }
    }

    // Abort the listener
    listener_handle.abort();
    forwarder_handle.abort();

    // Wait for active connections to drain
    drain_connections(
        &pool,
        std::time::Duration::from_secs(config.shutdown_timeout_secs),
    )
    .await;

    info!("Persistent proxy stopped");
    Ok(())
}

/// Run the proxy server in managed mode (web UI — CancellationToken + metrics channel).
///
/// When `capture_cmd_rx` is `None`, behavior is the single-capture legacy mode:
/// capture runs for the proxy lifetime and `Some(WorkloadProfile)` is returned on shutdown.
///
/// When `capture_cmd_rx` is `Some`, the proxy enters multi-capture mode:
/// capture is toggled via `CaptureCommand::Start` / `CaptureCommand::Stop`.
/// Each Stop writes a `.wkl` file and replies with a summary.
/// The proxy stays running until `cancel_token` is cancelled, returning `Ok(None)`.
pub async fn run_proxy_managed(
    config: ProxyConfig,
    cancel_token: CancellationToken,
    metrics_tx: mpsc::UnboundedSender<CaptureEvent>,
    capture_cmd_rx: Option<mpsc::UnboundedReceiver<CaptureCommand>>,
) -> Result<Option<WorkloadProfile>> {
    // Delegate to multi-capture path if a command channel was provided.
    if let Some(cmd_rx) = capture_cmd_rx {
        return run_proxy_managed_multi(config, cancel_token, metrics_tx, cmd_rx).await;
    }

    // ---------- Legacy single-capture path ----------
    let listener = socket::create_listener(&config.listen_addr, config.listen_backlog).await?;
    let pool = Arc::new(SessionPool::with_lifecycle(
        config.target_addr.clone(),
        config.pool_size,
        config.pool_timeout_secs,
        config.connect_timeout_secs,
        config.server_lifetime_secs,
        config.server_idle_timeout_secs,
    ));

    // Spawn idle reaper — cancelled when the pool is dropped or proxy shuts down
    let reaper_cancel = cancel_token.clone();
    let _reaper_handle = pool.spawn_idle_reaper(reaper_cancel);

    let (capture_tx, capture_rx) = mpsc::unbounded_channel();

    // Spawn capture collector
    let collector_handle = tokio::spawn(async move { run_collector(capture_rx).await });

    // Spawn listener with metrics channel
    let pool_clone = pool.clone();
    let no_capture = Arc::new(AtomicBool::new(config.no_capture));
    let enable_correlation = config.enable_correlation;
    let max_conns_per_ip_managed = config.max_connections_per_ip;
    let implicit_capture = build_implicit_capture_state(&config);
    let timeouts = build_timeout_config(&config);
    let tls_acceptor_managed = config.client_tls_acceptor.map(Arc::new);
    let listener_handle = tokio::spawn(async move {
        listener::run_listener(
            listener,
            pool_clone,
            capture_tx,
            no_capture,
            Some(metrics_tx),
            enable_correlation,
            implicit_capture,
            timeouts,
            max_conns_per_ip_managed,
            tls_acceptor_managed,
        )
        .await
    });

    // Wait for cancellation or duration
    match config.duration {
        Some(dur) => {
            tokio::select! {
                _ = tokio::time::sleep(dur) => {
                    info!("Proxy duration elapsed, shutting down...");
                }
                _ = cancel_token.cancelled() => {
                    info!("Proxy cancelled via web UI");
                }
            }
        }
        None => {
            cancel_token.cancelled().await;
            info!("Proxy cancelled via web UI");
        }
    }

    // Abort the listener to stop accepting new connections
    listener_handle.abort();

    // Wait for active connections to drain
    drain_connections(
        &pool,
        std::time::Duration::from_secs(config.shutdown_timeout_secs),
    )
    .await;

    // Wait for collector to finish (channel senders will be dropped)
    let captured = collector_handle.await?;

    if config.no_capture {
        info!("Proxy stopped (capture was disabled)");
        return Ok(None);
    }

    // Build profile
    let source_host = config.target_addr.clone();
    let profile = build_profile(captured, &source_host, config.mask_values);

    info!(
        "Captured {} queries across {} sessions",
        profile.metadata.total_queries, profile.metadata.total_sessions
    );

    // Write profile to disk
    if let Some(ref output) = config.output {
        io::write_profile(output, &profile)?;
        info!("Wrote workload profile to {}", output.display());
    }

    Ok(Some(profile))
}

/// Multi-capture managed proxy — capture is toggled via CaptureCommand messages.
///
/// Follows the same pattern as `run_proxy_persistent()` but integrates with the
/// web UI via CancellationToken + metrics channel instead of signal handling + control HTTP.
async fn run_proxy_managed_multi(
    config: ProxyConfig,
    cancel_token: CancellationToken,
    metrics_tx: mpsc::UnboundedSender<CaptureEvent>,
    mut cmd_rx: mpsc::UnboundedReceiver<CaptureCommand>,
) -> Result<Option<WorkloadProfile>> {
    let listener = socket::create_listener(&config.listen_addr, config.listen_backlog).await?;
    let pool = Arc::new(SessionPool::with_lifecycle(
        config.target_addr.clone(),
        config.pool_size,
        config.pool_timeout_secs,
        config.connect_timeout_secs,
        config.server_lifetime_secs,
        config.server_idle_timeout_secs,
    ));

    // Spawn idle reaper — cancelled when the proxy shuts down
    let reaper_cancel = cancel_token.clone();
    let _reaper_handle = pool.spawn_idle_reaper(reaper_cancel);

    // Shared no_capture flag — starts true (no capture until Start command).
    // Use the shared handle from config if provided (allows the web toggle handler
    // to verify capture state directly), otherwise create a local one.
    let no_capture = config
        .shared_no_capture
        .clone()
        .unwrap_or_else(|| Arc::new(AtomicBool::new(true)));
    no_capture.store(true, Ordering::Relaxed);

    // Open staging DB in the output directory (or a temp location)
    let staging_path = config
        .output
        .as_ref()
        .and_then(|p| p.parent())
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("proxy-capture-managed.db");
    let staging_db = StagingDb::open_standalone(&staging_path).await?;

    // Persistent capture channel — listener writes here for the proxy lifetime.
    let (persistent_capture_tx, persistent_capture_rx) = mpsc::unbounded_channel::<CaptureEvent>();

    let pool_clone = pool.clone();
    let no_capture_clone = no_capture.clone();
    let enable_correlation = config.enable_correlation;
    let max_conns_per_ip_multi = config.max_connections_per_ip;
    let implicit_capture = build_implicit_capture_state(&config);
    let timeouts = build_timeout_config(&config);
    let tls_acceptor_multi = config.client_tls_acceptor.map(Arc::new);
    let listener_handle = tokio::spawn(async move {
        listener::run_listener(
            listener,
            pool_clone,
            persistent_capture_tx,
            no_capture_clone,
            Some(metrics_tx),
            enable_correlation,
            implicit_capture,
            timeouts,
            max_conns_per_ip_multi,
            tls_acceptor_multi,
        )
        .await
    });

    // Intermediary: forwarder sends events to the active staging collector (if any).
    let active_staging_tx: Arc<tokio::sync::Mutex<Option<mpsc::UnboundedSender<CaptureEvent>>>> =
        Arc::new(tokio::sync::Mutex::new(None));

    let active_staging_tx_clone = active_staging_tx.clone();
    let forwarder_handle = tokio::spawn(async move {
        let mut rx = persistent_capture_rx;
        while let Some(event) = rx.recv().await {
            let guard = active_staging_tx_clone.lock().await;
            if let Some(ref tx) = *guard {
                let _ = tx.send(event);
            }
        }
    });

    // Track the active collector task
    let mut active_collector: Option<tokio::task::JoinHandle<()>> = None;
    let mut active_capture_id: Option<String> = None;

    info!(
        "Managed proxy (multi-capture) running on {} (target: {})",
        config.listen_addr, config.target_addr
    );

    // Main command loop — also listen for cancel_token
    loop {
        let cmd = tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(c) => c,
                    None => break, // Command channel closed
                }
            }
            _ = cancel_token.cancelled() => {
                info!("Proxy cancelled via web UI");
                break;
            }
        };

        match cmd {
            CaptureCommand::Start { reply } => {
                if active_capture_id.is_some() {
                    let _ = reply.send(Err("Already capturing".to_string()));
                    continue;
                }

                let capture_id = Uuid::new_v4().to_string();
                info!("Starting capture session: {capture_id}");

                // Create a new staging collector channel
                let (staging_tx, staging_rx) = mpsc::unbounded_channel::<CaptureEvent>();

                // Set the forwarder to send to this new staging collector
                {
                    let mut guard = active_staging_tx.lock().await;
                    *guard = Some(staging_tx);
                }

                // Spawn staging collector
                let db_clone = staging_db.clone();
                let cap_id_clone = capture_id.clone();
                active_collector = Some(tokio::spawn(async move {
                    run_staging_collector(staging_rx, db_clone, cap_id_clone).await;
                }));

                // Enable capture
                no_capture.store(false, Ordering::Relaxed);

                active_capture_id = Some(capture_id.clone());
                let _ = reply.send(Ok(capture_id));
            }
            CaptureCommand::Stop { output, reply } => {
                let capture_id = match active_capture_id.take() {
                    Some(id) => id,
                    None => {
                        let _ = reply.send(Err("Not currently capturing".to_string()));
                        continue;
                    }
                };

                info!("Stopping capture session: {capture_id}");

                // Disable capture (stop emitting events)
                no_capture.store(true, Ordering::Relaxed);

                // Close the staging collector channel by clearing the forwarder target
                {
                    let mut guard = active_staging_tx.lock().await;
                    *guard = None;
                }

                // Wait for the collector to finish processing
                if let Some(handle) = active_collector.take() {
                    let _ = handle.await;
                }

                // Read staged data and build profile
                let rows = match staging_db.read_capture(&capture_id).await {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = reply.send(Err(format!("Failed to read staging data: {e}")));
                        continue;
                    }
                };

                let profile =
                    build_profile_from_staging(rows, &config.target_addr, config.mask_values);

                info!(
                    "Captured {} queries across {} sessions",
                    profile.metadata.total_queries, profile.metadata.total_sessions
                );

                // Determine output path
                let output_path = output.or_else(|| config.output.clone()).unwrap_or_else(|| {
                    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                    PathBuf::from(format!("capture-{ts}.wkl"))
                });

                // Write profile
                if let Err(e) = io::write_profile(&output_path, &profile) {
                    let _ = reply.send(Err(format!("Failed to write profile: {e}")));
                    let _ = staging_db.clear_capture(&capture_id).await;
                    continue;
                }

                info!("Wrote workload profile to {}", output_path.display());

                // Clear staging data
                let _ = staging_db.clear_capture(&capture_id).await;

                let _ = reply.send(Ok(serde_json::json!({
                    "ok": true,
                    "capture_id": capture_id,
                    "output": output_path.to_string_lossy(),
                    "total_sessions": profile.metadata.total_sessions,
                    "total_queries": profile.metadata.total_queries,
                })));
            }
        }
    }

    // Shutdown: if we were capturing, flush the workload to disk before exiting
    if let Some(capture_id) = active_capture_id.take() {
        info!("Flushing active capture {capture_id} during shutdown...");
        no_capture.store(true, Ordering::Relaxed);
        {
            let mut guard = active_staging_tx.lock().await;
            *guard = None;
        }
        if let Some(handle) = active_collector.take() {
            let _ = handle.await;
        }

        // Read staged data and build profile
        match staging_db.read_capture(&capture_id).await {
            Ok(rows) => {
                let profile =
                    build_profile_from_staging(rows, &config.target_addr, config.mask_values);
                info!(
                    "Captured {} queries across {} sessions",
                    profile.metadata.total_queries, profile.metadata.total_sessions
                );

                let output_path = config.output.clone().unwrap_or_else(|| {
                    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
                    PathBuf::from(format!("capture-{ts}.wkl"))
                });

                match io::write_profile(&output_path, &profile) {
                    Ok(()) => info!("Wrote workload profile to {}", output_path.display()),
                    Err(e) => warn!("Failed to write profile during shutdown: {e}"),
                }

                let _ = staging_db.clear_capture(&capture_id).await;
            }
            Err(e) => {
                warn!("Failed to read staging data during shutdown: {e}");
            }
        }
    }

    // Abort the listener
    listener_handle.abort();
    forwarder_handle.abort();

    // Wait for active connections to drain
    drain_connections(
        &pool,
        std::time::Duration::from_secs(config.shutdown_timeout_secs),
    )
    .await;

    info!("Managed proxy (multi-capture) stopped");
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_drain_connections_immediate_when_no_active() {
        let pool = SessionPool::new("127.0.0.1:5432".to_string(), 10, 30, 5);
        let start = Instant::now();
        drain_connections(&pool, Duration::from_secs(5)).await;
        // Should return almost immediately (well under 1s)
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn test_drain_connections_timeout_with_active() {
        let pool = SessionPool::new("127.0.0.1:5432".to_string(), 10, 30, 5);

        // Simulate an active connection
        pool.set_active_count(1).await;

        let start = Instant::now();
        // Use a short timeout (1s) — drain should wait and then give up
        drain_connections(&pool, Duration::from_secs(1)).await;
        let elapsed = start.elapsed();

        // Should have waited at least ~1s (the timeout) but not much more
        assert!(
            elapsed >= Duration::from_millis(900),
            "Expected at least ~1s wait, got {:?}",
            elapsed
        );
        assert!(
            elapsed < Duration::from_secs(3),
            "Should not exceed timeout by much, got {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn test_drain_connections_exits_when_active_drops_to_zero() {
        let pool = Arc::new(SessionPool::new("127.0.0.1:5432".to_string(), 10, 30, 5));

        // Start with 1 active connection
        pool.set_active_count(1).await;

        // Spawn a task that "finishes" the connection after 500ms
        let pool_clone = pool.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            pool_clone.discard().await;
        });

        let start = Instant::now();
        drain_connections(&pool, Duration::from_secs(30)).await;
        let elapsed = start.elapsed();

        // Should have finished around 500-750ms (after the discard), not the full 30s
        assert!(
            elapsed < Duration::from_secs(2),
            "Should exit early when active reaches 0, got {:?}",
            elapsed
        );
        assert!(
            elapsed >= Duration::from_millis(400),
            "Should wait until active drops, got {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn test_drain_connections_zero_timeout() {
        let pool = SessionPool::new("127.0.0.1:5432".to_string(), 10, 30, 5);

        // Simulate active connections
        pool.set_active_count(5).await;

        let start = Instant::now();
        drain_connections(&pool, Duration::from_secs(0)).await;
        // Should return immediately (0 timeout = force close)
        assert!(start.elapsed() < Duration::from_secs(1));
    }
}
