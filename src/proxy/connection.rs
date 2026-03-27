use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use tokio::io::{AsyncWriteExt, BufReader, BufWriter, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, warn};

use super::capture::CaptureEvent;
use super::pool::SessionPool;
use super::protocol::{
    self, build_query_message, extract_bind, extract_data_row, extract_error_message,
    extract_parse, extract_query_sql, extract_row_description, format_bind_params,
    inline_bind_params, StartupType,
};
use crate::correlate::capture::{has_returning, inject_returning, is_currval_or_lastval, TablePk};

/// Shared state for RETURNING clause detection and DataRow capture.
///
/// Created when `IdMode::Correlate` or `IdMode::Full` is active.
/// Tracks a queue of booleans (one per query/parse) indicating whether
/// the query has a RETURNING clause, and accumulates column descriptions
/// and row data from the server response.
pub struct CorrelateState {
    /// FIFO of booleans: true if the query at this position has RETURNING.
    returning_queue: TokioMutex<VecDeque<bool>>,
    /// Column names from the most recent RowDescription (if RETURNING).
    pending_columns: TokioMutex<Vec<String>>,
    /// Accumulated data rows from DataRow messages (if RETURNING).
    pending_rows: TokioMutex<Vec<Vec<Option<String>>>>,
    /// When true, the server-to-client relay should suppress 'T' (RowDescription)
    /// and 'D' (DataRow) messages — they came from auto-injected RETURNING
    /// and the client doesn't expect them.
    suppress_response: TokioMutex<bool>,
}

impl Default for CorrelateState {
    fn default() -> Self {
        Self::new()
    }
}

impl CorrelateState {
    pub fn new() -> Self {
        Self {
            returning_queue: TokioMutex::new(VecDeque::new()),
            pending_columns: TokioMutex::new(Vec::new()),
            pending_rows: TokioMutex::new(Vec::new()),
            suppress_response: TokioMutex::new(false),
        }
    }
}

/// Shared state for implicit RETURNING injection (`--id-capture-implicit`).
///
/// Holds the primary key map discovered at proxy startup. This is shared
/// across all connections (read-only after construction).
pub struct ImplicitCaptureState {
    /// Primary key columns per table, used by `inject_returning()`.
    pub pk_map: Vec<TablePk>,
    /// When true (default), suppress auto-injected RETURNING results from the client.
    /// The client sees the same response as a bare INSERT (CommandComplete only).
    pub stealth: bool,
}

/// Timeout configuration for proxy connections.
#[derive(Clone, Debug)]
pub struct TimeoutConfig {
    /// Client idle read timeout. `None` means no timeout.
    pub client_timeout: Option<Duration>,
    /// Server idle read timeout. `None` means no timeout.
    pub server_timeout: Option<Duration>,
    /// Authentication phase timeout. `None` means no timeout.
    pub auth_timeout: Option<Duration>,
}

impl TimeoutConfig {
    /// Build from seconds values. 0 means no timeout.
    pub fn from_secs(client: u64, server: u64, auth: u64) -> Self {
        Self {
            client_timeout: if client > 0 {
                Some(Duration::from_secs(client))
            } else {
                None
            },
            server_timeout: if server > 0 {
                Some(Duration::from_secs(server))
            } else {
                None
            },
            auth_timeout: if auth > 0 {
                Some(Duration::from_secs(auth))
            } else {
                None
            },
        }
    }
}

/// Handle a single client connection through its full lifecycle.
#[allow(clippy::too_many_arguments)]
pub async fn handle_connection(
    client_stream: TcpStream,
    pool: Arc<SessionPool>,
    session_id: u64,
    capture_tx: mpsc::UnboundedSender<CaptureEvent>,
    no_capture: Arc<AtomicBool>,
    metrics_tx: Option<mpsc::UnboundedSender<CaptureEvent>>,
    correlate: Option<Arc<CorrelateState>>,
    implicit_capture: Option<Arc<ImplicitCaptureState>>,
    timeouts: TimeoutConfig,
) {
    if let Err(e) = handle_connection_inner(
        client_stream,
        pool,
        session_id,
        capture_tx,
        no_capture,
        metrics_tx,
        correlate,
        implicit_capture,
        timeouts,
    )
    .await
    {
        debug!("Session {session_id} ended: {e}");
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_connection_inner(
    mut client_stream: TcpStream,
    pool: Arc<SessionPool>,
    session_id: u64,
    capture_tx: mpsc::UnboundedSender<CaptureEvent>,
    no_capture: Arc<AtomicBool>,
    metrics_tx: Option<mpsc::UnboundedSender<CaptureEvent>>,
    correlate: Option<Arc<CorrelateState>>,
    implicit_capture: Option<Arc<ImplicitCaptureState>>,
    timeouts: TimeoutConfig,
) -> Result<()> {
    // ── Phase 1: Startup ────────────────────────────────────────────
    // Read the first message from client (no type byte — startup message)
    let startup_msg = match protocol::read_startup_message(&mut client_stream).await? {
        Some(msg) => msg,
        None => return Ok(()), // Client disconnected immediately
    };

    // Handle SSLRequest
    let startup_msg = match protocol::classify_startup(&startup_msg) {
        StartupType::SslRequest => {
            // Reject SSL — respond with 'N'
            client_stream.write_all(b"N").await?;
            // Client should now send actual StartupMessage
            match protocol::read_startup_message(&mut client_stream).await? {
                Some(msg) => msg,
                None => return Ok(()),
            }
        }
        StartupType::CancelRequest => {
            debug!("Session {session_id}: cancel request (not yet supported)");
            return Ok(());
        }
        StartupType::StartupMessage => startup_msg,
        StartupType::Unknown => {
            warn!("Session {session_id}: unknown startup message");
            return Ok(());
        }
    };

    // Extract user/database from startup
    let (user, database) = protocol::parse_startup_params(&startup_msg);
    let user = user.unwrap_or_else(|| "unknown".to_string());
    let database = database.unwrap_or_else(|| user.clone());

    debug!("Session {session_id}: startup user={user} database={database}");

    // ── Phase 2: Get server connection from pool ────────────────────
    let server_conn = pool.checkout().await?;
    let mut server_stream = server_conn.stream;

    // Forward startup message to server
    protocol::write_message(&mut server_stream, &startup_msg).await?;

    // ── Phase 3: Auth passthrough (with optional timeout) ──────────
    let auth_complete = if let Some(auth_dur) = timeouts.auth_timeout {
        match tokio::time::timeout(auth_dur, relay_auth(&mut client_stream, &mut server_stream))
            .await
        {
            Ok(result) => result?,
            Err(_) => {
                warn!("Session {session_id}: auth timeout after {auth_dur:?}");
                pool.discard().await;
                bail!("auth timeout");
            }
        }
    } else {
        relay_auth(&mut client_stream, &mut server_stream).await?
    };
    if !auth_complete {
        pool.discard().await;
        return Ok(());
    }

    // Always send session start — collector needs user/database metadata
    // even if capture is toggled on later for this session
    {
        let event = CaptureEvent::SessionStart {
            session_id,
            user,
            database,
            timestamp: Instant::now(),
        };
        if let Some(ref mtx) = metrics_tx {
            let _ = mtx.send(event.clone());
        }
        let _ = capture_tx.send(event);
    }

    // ── Phase 4: Bidirectional relay with capture ───────────────────
    let (client_read, client_write) = tokio::io::split(client_stream);
    let (server_read, server_write) = tokio::io::split(server_stream);
    // Buffer I/O to batch small protocol reads/writes
    let client_read = BufReader::new(client_read);
    let client_write = BufWriter::new(client_write);
    let server_read = BufReader::new(server_read);
    let server_write = BufWriter::new(server_write);

    let capture_tx2 = capture_tx.clone();
    let metrics_tx2 = metrics_tx.clone();

    // Shared state for prepared statement tracking
    let stmt_cache: Arc<tokio::sync::Mutex<HashMap<String, String>>> =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));

    // Client → Server relay
    let client_idle_timeout = timeouts.client_timeout;
    let server_idle_timeout = timeouts.server_timeout;

    let c2s = tokio::spawn({
        let capture_tx = capture_tx.clone();
        let metrics_tx = metrics_tx.clone();
        let stmt_cache = stmt_cache.clone();
        let no_capture = no_capture.clone();
        let correlate = correlate.clone();
        let implicit_capture = implicit_capture.clone();
        async move {
            relay_client_to_server(
                client_read,
                server_write,
                session_id,
                capture_tx,
                stmt_cache,
                no_capture,
                metrics_tx,
                correlate,
                implicit_capture,
                client_idle_timeout,
            )
            .await
        }
    });

    // Server → Client relay
    let s2c = tokio::spawn(async move {
        relay_server_to_client(
            server_read,
            client_write,
            session_id,
            capture_tx2,
            no_capture,
            metrics_tx2,
            correlate,
            server_idle_timeout,
        )
        .await
    });

    // Wait for either direction to finish (one side disconnected)
    tokio::select! {
        result = c2s => {
            if let Ok(Err(e)) = result {
                debug!("Session {session_id}: c2s error: {e}");
            }
        }
        result = s2c => {
            if let Ok(Err(e)) = result {
                debug!("Session {session_id}: s2c error: {e}");
            }
        }
    }

    // Session complete — discard the server connection
    // (connection may be in unknown state after one side disconnected)
    pool.discard().await;

    Ok(())
}

/// Relay auth messages between client and server until ReadyForQuery.
/// Returns true if auth succeeded, false if the connection was lost.
async fn relay_auth(client: &mut TcpStream, server: &mut TcpStream) -> Result<bool> {
    loop {
        // Read server response
        let msg = match protocol::read_message(server).await? {
            Some(m) => m,
            None => return Ok(false),
        };

        let is_ready = msg.msg_type == b'Z';
        let is_auth_request = msg.msg_type == b'R';

        // Forward to client
        protocol::write_message(client, &msg).await?;

        if is_ready {
            return Ok(true); // Auth complete, ready for queries
        }

        // If server sent an auth request, check if client needs to respond
        if is_auth_request {
            let body = msg.body();
            if body.len() >= 4 {
                let auth_type = i32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                match auth_type {
                    // AuthenticationOk — done with auth, server sends params next
                    0 => continue,
                    // AuthenticationSASLFinal — server verification, no client response
                    12 => continue,
                    // All others expect a client response:
                    // 3=Cleartext, 5=MD5, 7=GSSAPI, 8=SSPI,
                    // 10=SASL, 11=SASLContinue
                    _ => {
                        if let Some(client_msg) = protocol::read_message(client).await? {
                            protocol::write_message(server, &client_msg).await?;
                        } else {
                            return Ok(false);
                        }
                    }
                }
            }
        }
    }
}

/// Read a protocol message with an optional idle timeout.
///
/// Returns `Ok(Some(msg))` on success, `Ok(None)` on disconnect or timeout.
/// On timeout, logs a warning and returns `None` to close the relay loop gracefully.
async fn read_with_timeout<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    timeout: Option<Duration>,
    session_id: u64,
    side: &str,
) -> Result<Option<protocol::PgMessage>> {
    if let Some(dur) = timeout {
        match tokio::time::timeout(dur, protocol::read_message(reader)).await {
            Ok(result) => result,
            Err(_) => {
                warn!(
                    "Session {session_id}: {side} idle timeout after {dur:?}, closing connection"
                );
                Ok(None)
            }
        }
    } else {
        protocol::read_message(reader).await
    }
}

/// Relay messages from client to server, extracting capture data.
#[allow(clippy::too_many_arguments)]
async fn relay_client_to_server(
    mut client: BufReader<ReadHalf<TcpStream>>,
    mut server: BufWriter<WriteHalf<TcpStream>>,
    session_id: u64,
    capture_tx: mpsc::UnboundedSender<CaptureEvent>,
    stmt_cache: Arc<tokio::sync::Mutex<HashMap<String, String>>>,
    no_capture: Arc<AtomicBool>,
    metrics_tx: Option<mpsc::UnboundedSender<CaptureEvent>>,
    correlate: Option<Arc<CorrelateState>>,
    implicit_capture: Option<Arc<ImplicitCaptureState>>,
    idle_timeout: Option<Duration>,
) -> Result<()> {
    loop {
        let msg = match read_with_timeout(&mut client, idle_timeout, session_id, "client").await? {
            Some(m) => m,
            None => break, // Client disconnected or timed out
        };

        if !no_capture.load(Ordering::Relaxed) {
            match msg.msg_type {
                b'Q' => {
                    // Simple query
                    if let Some(sql) = extract_query_sql(&msg) {
                        // Implicit capture: check for currval/lastval or inject RETURNING
                        let (capture_sql, has_ret) = if let Some(ref ic) = implicit_capture {
                            if is_currval_or_lastval(&sql) {
                                // currval/lastval returns a value we want to correlate
                                (sql.clone(), true)
                            } else if let Some(injected) = inject_returning(&sql, &ic.pk_map) {
                                // Bare INSERT → inject RETURNING, rewrite the message sent to server
                                let rewritten = build_query_message(&injected);
                                protocol::write_message(&mut server, &rewritten).await?;
                                server.flush().await?;

                                // Track RETURNING for correlation and set suppress flag
                                // for stealth mode (hide injected RETURNING results from client)
                                if let Some(ref cs) = correlate {
                                    cs.returning_queue.lock().await.push_back(true);
                                    if ic.stealth {
                                        *cs.suppress_response.lock().await = true;
                                    }
                                }
                                let event = CaptureEvent::QueryStart {
                                    session_id,
                                    sql,
                                    timestamp: Instant::now(),
                                };
                                if let Some(ref mtx) = metrics_tx {
                                    let _ = mtx.send(event.clone());
                                }
                                let _ = capture_tx.send(event);
                                // Skip the normal write_message at the end — we already sent the rewritten query
                                continue;
                            } else {
                                (sql.clone(), has_returning(&sql))
                            }
                        } else {
                            (sql.clone(), has_returning(&sql))
                        };

                        // Track RETURNING for correlation
                        if let Some(ref cs) = correlate {
                            cs.returning_queue.lock().await.push_back(has_ret);
                        }
                        let event = CaptureEvent::QueryStart {
                            session_id,
                            sql: capture_sql,
                            timestamp: Instant::now(),
                        };
                        if let Some(ref mtx) = metrics_tx {
                            let _ = mtx.send(event.clone());
                        }
                        let _ = capture_tx.send(event);
                    }
                }
                b'P' => {
                    // Parse (prepared statement) — cache name→SQL mapping
                    if let Some(parsed) = extract_parse(&msg) {
                        // Track RETURNING for correlation
                        if let Some(ref cs) = correlate {
                            let has_ret = if let Some(ref ic) = implicit_capture {
                                is_currval_or_lastval(&parsed.sql)
                                    || has_returning(&parsed.sql)
                                    || inject_returning(&parsed.sql, &ic.pk_map).is_some()
                            } else {
                                has_returning(&parsed.sql)
                            };
                            cs.returning_queue.lock().await.push_back(has_ret);
                        }
                        let mut cache = stmt_cache.lock().await;
                        cache.insert(parsed.statement_name, parsed.sql);
                    }
                }
                b'B' => {
                    // Bind — resolve stmt name to SQL, inline params
                    if let Some(bind) = extract_bind(&msg) {
                        let cache = stmt_cache.lock().await;
                        if let Some(sql_template) = cache.get(&bind.statement_name) {
                            let params = format_bind_params(&bind.parameters);
                            let sql = inline_bind_params(sql_template, &params);
                            let event = CaptureEvent::QueryStart {
                                session_id,
                                sql,
                                timestamp: Instant::now(),
                            };
                            if let Some(ref mtx) = metrics_tx {
                                let _ = mtx.send(event.clone());
                            }
                            let _ = capture_tx.send(event);
                        }
                    }
                }
                b'X' => {
                    // Terminate
                    protocol::write_message(&mut server, &msg).await?;
                    server.flush().await?;
                    let event = CaptureEvent::SessionEnd { session_id };
                    if let Some(ref mtx) = metrics_tx {
                        let _ = mtx.send(event.clone());
                    }
                    let _ = capture_tx.send(event);
                    break;
                }
                _ => {}
            }
        } else if msg.msg_type == b'X' {
            protocol::write_message(&mut server, &msg).await?;
            server.flush().await?;
            break;
        }

        protocol::write_message(&mut server, &msg).await?;
        server.flush().await?;
    }
    Ok(())
}

/// Maximum number of columns to capture from RETURNING clause.
const MAX_RETURNING_COLUMNS: usize = 20;
/// Maximum number of rows to capture from RETURNING clause.
const MAX_RETURNING_ROWS: usize = 100;

/// Relay messages from server to client, extracting capture data.
#[allow(clippy::too_many_arguments)]
async fn relay_server_to_client(
    mut server: BufReader<ReadHalf<TcpStream>>,
    mut client: BufWriter<WriteHalf<TcpStream>>,
    session_id: u64,
    capture_tx: mpsc::UnboundedSender<CaptureEvent>,
    no_capture: Arc<AtomicBool>,
    metrics_tx: Option<mpsc::UnboundedSender<CaptureEvent>>,
    correlate: Option<Arc<CorrelateState>>,
    idle_timeout: Option<Duration>,
) -> Result<()> {
    loop {
        let msg = match read_with_timeout(&mut server, idle_timeout, session_id, "server").await? {
            Some(m) => m,
            None => break, // Server disconnected or timed out
        };

        if !no_capture.load(Ordering::Relaxed) {
            match msg.msg_type {
                b'T' => {
                    // RowDescription — if correlation is active and front of queue is true,
                    // capture column names for RETURNING clause.
                    if let Some(ref cs) = correlate {
                        let queue = cs.returning_queue.lock().await;
                        if queue.front() == Some(&true) {
                            if let Some(mut columns) = extract_row_description(&msg) {
                                columns.truncate(MAX_RETURNING_COLUMNS);
                                let mut pending = cs.pending_columns.lock().await;
                                *pending = columns;
                                // Clear any stale rows
                                cs.pending_rows.lock().await.clear();
                            }
                        }
                    }
                }
                b'D' => {
                    // DataRow — if we have pending columns (RETURNING active), accumulate row data.
                    if let Some(ref cs) = correlate {
                        let pending_cols = cs.pending_columns.lock().await;
                        if !pending_cols.is_empty() {
                            let num_cols = pending_cols.len();
                            drop(pending_cols); // Release lock before acquiring rows lock
                            if let Some(values) = extract_data_row(&msg, num_cols) {
                                let mut rows = cs.pending_rows.lock().await;
                                if rows.len() < MAX_RETURNING_ROWS {
                                    rows.push(values);
                                }
                            }
                        }
                    }
                }
                b'C' => {
                    // CommandComplete — query finished.
                    // IMPORTANT: QueryComplete must be sent BEFORE QueryReturning,
                    // because the collector moves pending_sql into queries on
                    // QueryComplete, and QueryReturning attaches to the last
                    // query in the queries vec.
                    let event = CaptureEvent::QueryComplete {
                        session_id,
                        timestamp: Instant::now(),
                    };
                    if let Some(ref mtx) = metrics_tx {
                        let _ = mtx.send(event.clone());
                    }
                    let _ = capture_tx.send(event);

                    // If correlation is active, pop the queue and emit QueryReturning if needed.
                    if let Some(ref cs) = correlate {
                        let was_returning =
                            cs.returning_queue.lock().await.pop_front().unwrap_or(false);
                        if was_returning {
                            let columns = {
                                let mut pending = cs.pending_columns.lock().await;
                                std::mem::take(&mut *pending)
                            };
                            let rows = {
                                let mut pending = cs.pending_rows.lock().await;
                                std::mem::take(&mut *pending)
                            };
                            if !rows.is_empty() {
                                debug!(
                                    "Session {session_id}: emitting QueryReturning with {} columns, {} rows",
                                    columns.len(),
                                    rows.len()
                                );
                                let event = CaptureEvent::QueryReturning {
                                    session_id,
                                    columns,
                                    rows,
                                    timestamp: Instant::now(),
                                };
                                if let Some(ref mtx) = metrics_tx {
                                    let _ = mtx.send(event.clone());
                                }
                                let _ = capture_tx.send(event);
                            }
                        }
                    }
                }
                b'E' => {
                    // ErrorResponse
                    // Clear correlation state and suppress flag on error
                    if let Some(ref cs) = correlate {
                        let _ = cs.returning_queue.lock().await.pop_front();
                        cs.pending_columns.lock().await.clear();
                        cs.pending_rows.lock().await.clear();
                        *cs.suppress_response.lock().await = false;
                    }

                    if let Some(err_msg) = extract_error_message(&msg) {
                        let event = CaptureEvent::QueryError {
                            session_id,
                            message: err_msg,
                            timestamp: Instant::now(),
                        };
                        if let Some(ref mtx) = metrics_tx {
                            let _ = mtx.send(event.clone());
                        }
                        let _ = capture_tx.send(event);
                    }
                }
                _ => {}
            }
        }

        // Stealth RETURNING mode: suppress RowDescription and DataRow messages
        // from auto-injected RETURNING so the client never sees them.
        // The capture logic above still runs — we just skip forwarding to the client.
        let suppress = if let Some(ref cs) = correlate {
            *cs.suppress_response.lock().await
        } else {
            false
        };

        match msg.msg_type {
            b'T' | b'D' if suppress => {
                // Captured for ID map above, but NOT forwarded to client
                continue;
            }
            b'C' if suppress => {
                // CommandComplete — clear suppress flag, then forward normally
                if let Some(ref cs) = correlate {
                    *cs.suppress_response.lock().await = false;
                }
            }
            _ => {}
        }

        protocol::write_message(&mut client, &msg).await?;
        // Flush after ReadyForQuery so client sees results promptly
        if msg.msg_type == b'Z' {
            client.flush().await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_suppress_response_flag_lifecycle() {
        // Verify that the suppress flag is set and cleared correctly
        let cs = CorrelateState::new();

        // Initially false
        assert!(!*cs.suppress_response.lock().await);

        // Set to true (simulating inject_returning in c2s relay)
        *cs.suppress_response.lock().await = true;
        assert!(*cs.suppress_response.lock().await);

        // Clear on CommandComplete (simulating s2c relay)
        *cs.suppress_response.lock().await = false;
        assert!(!*cs.suppress_response.lock().await);
    }

    #[tokio::test]
    async fn test_suppress_clears_on_error() {
        // Verify that the suppress flag is cleared when an error occurs
        let cs = CorrelateState::new();
        cs.returning_queue.lock().await.push_back(true);
        *cs.suppress_response.lock().await = true;

        // Simulate ErrorResponse handling: pop queue, clear pending state, clear suppress
        let _ = cs.returning_queue.lock().await.pop_front();
        cs.pending_columns.lock().await.clear();
        cs.pending_rows.lock().await.clear();
        *cs.suppress_response.lock().await = false;

        assert!(!*cs.suppress_response.lock().await);
        assert!(cs.returning_queue.lock().await.is_empty());
    }

    #[tokio::test]
    async fn test_stealth_suppresses_t_and_d_but_forwards_c() {
        // Simulate the message type matching logic used in relay_server_to_client.
        // When suppress is true: T and D should be skipped, C should clear the flag.
        let cs = CorrelateState::new();
        *cs.suppress_response.lock().await = true;

        let mut forwarded: Vec<u8> = Vec::new();
        let msg_types = vec![b'T', b'D', b'D', b'C', b'Z'];

        for msg_type in msg_types {
            let suppress = *cs.suppress_response.lock().await;

            match msg_type {
                b'T' | b'D' if suppress => {
                    // Suppressed — not forwarded
                    continue;
                }
                b'C' if suppress => {
                    // CommandComplete — clear suppress, forward
                    *cs.suppress_response.lock().await = false;
                }
                _ => {}
            }

            forwarded.push(msg_type);
        }

        // Only C and Z should be forwarded; T and D are suppressed
        assert_eq!(forwarded, vec![b'C', b'Z']);
        // Suppress flag should be cleared after C
        assert!(!*cs.suppress_response.lock().await);
    }

    #[tokio::test]
    async fn test_no_suppress_when_flag_is_false() {
        // When suppress is false (explicit RETURNING or stealth disabled),
        // all messages should be forwarded.
        let cs = CorrelateState::new();
        // suppress_response is false by default

        let mut forwarded: Vec<u8> = Vec::new();
        let msg_types = vec![b'T', b'D', b'D', b'C', b'Z'];

        for msg_type in msg_types {
            let suppress = *cs.suppress_response.lock().await;

            match msg_type {
                b'T' | b'D' if suppress => {
                    continue;
                }
                b'C' if suppress => {
                    *cs.suppress_response.lock().await = false;
                }
                _ => {}
            }

            forwarded.push(msg_type);
        }

        // All messages forwarded
        assert_eq!(forwarded, vec![b'T', b'D', b'D', b'C', b'Z']);
    }

    #[test]
    fn test_implicit_capture_state_stealth_default() {
        // Stealth should be true by default (no_stealth = false)
        let state = ImplicitCaptureState {
            pk_map: vec![],
            stealth: true,
        };
        assert!(state.stealth);

        // When --no-stealth is passed
        let state = ImplicitCaptureState {
            pk_map: vec![],
            stealth: false,
        };
        assert!(!state.stealth);
    }

    #[test]
    fn test_timeout_config_from_secs_zero_means_no_timeout() {
        let tc = TimeoutConfig::from_secs(0, 0, 0);
        assert!(tc.client_timeout.is_none());
        assert!(tc.server_timeout.is_none());
        assert!(tc.auth_timeout.is_none());
    }

    #[test]
    fn test_timeout_config_from_secs_nonzero() {
        let tc = TimeoutConfig::from_secs(300, 600, 30);
        assert_eq!(tc.client_timeout, Some(Duration::from_secs(300)));
        assert_eq!(tc.server_timeout, Some(Duration::from_secs(600)));
        assert_eq!(tc.auth_timeout, Some(Duration::from_secs(30)));
    }

    #[test]
    fn test_timeout_config_mixed() {
        // Only auth timeout enabled
        let tc = TimeoutConfig::from_secs(0, 0, 15);
        assert!(tc.client_timeout.is_none());
        assert!(tc.server_timeout.is_none());
        assert_eq!(tc.auth_timeout, Some(Duration::from_secs(15)));
    }

    #[tokio::test]
    async fn test_read_with_timeout_no_timeout_returns_none_on_eof() {
        // Simulate an empty reader (EOF) with no timeout
        let data: &[u8] = &[];
        let mut cursor = std::io::Cursor::new(data);
        let result = read_with_timeout(&mut cursor, None, 1, "test").await;
        // EOF produces Ok(None)
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_read_with_timeout_triggers_on_empty_reader() {
        // Simulate a timeout with a reader that never returns data.
        // We use a tokio::io::pending() which never completes.
        let timeout = Some(Duration::from_millis(50));
        let mut reader = tokio::io::empty(); // Returns EOF immediately
        let result = read_with_timeout(&mut reader, timeout, 1, "test").await;
        // empty() returns EOF, so we get Ok(None) rather than timeout
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_read_with_timeout_fires_on_stalled_reader() {
        // Create a reader that blocks forever using duplex with a held sender.
        let timeout = Some(Duration::from_millis(50));

        // Hold both ends so the reader blocks (no EOF, no data)
        let (tx, rx) = tokio::io::duplex(64);
        let mut reader = tokio::io::BufReader::new(rx);

        let start = std::time::Instant::now();
        let result = read_with_timeout(&mut reader, timeout, 42, "test").await;
        let elapsed = start.elapsed();

        // Should timeout and return Ok(None)
        assert!(result.unwrap().is_none());
        // Should have taken roughly 50ms (not zero, not 300s)
        assert!(elapsed >= Duration::from_millis(40));
        assert!(elapsed < Duration::from_secs(1));

        // Keep tx alive to prevent EOF
        drop(tx);
    }
}
