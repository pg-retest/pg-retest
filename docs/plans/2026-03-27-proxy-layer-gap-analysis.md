# Proxy Layer Gap Analysis: pg-retest vs Production Proxy Best Practices

**Date:** 2026-03-27
**Sources:** HAProxy 2.9+, ProxySQL 3.x, PgBouncer 1.23+
**Scope:** Network/proxy layer hardening only — not capture logic, replay, or application features

---

## Executive Summary

pg-retest's proxy is architecturally sound for its capture-and-replay purpose: per-connection tokio tasks, session-mode pooling, full PG protocol v3 parsing, and bidirectional relay with capture hooks. However, compared to HAProxy, ProxySQL, and PgBouncer, it lacks the operational hardening that production proxies treat as table stakes: **timeouts, resource limits, TCP tuning, health checks, and observability**. These gaps don't affect correctness but they expose the proxy to hung connections, resource exhaustion, silent failures, and difficulty diagnosing issues under load.

The gaps below are ordered by severity (how likely they are to cause real problems in production-adjacent usage).

---

## Gap Analysis

### CRITICAL — Will cause problems under real traffic

| # | Gap | What pg-retest Does | What Production Proxies Do | Risk |
|---|-----|---------------------|---------------------------|------|
| C1 | **No read/write timeouts on relay** | Relay loops (`relay_client_to_server`, `relay_server_to_client`) block indefinitely on `read_message()` | HAProxy: `timeout client`/`timeout server` (30-50s). PgBouncer: `server_connect_timeout` (5s), `query_timeout`, `client_idle_timeout`. ProxySQL: `connect_timeout_server` (1s), `default_query_timeout` (24h), `ping_timeout_server` (200ms) | A stuck client or crashed server holds a connection + tokio task + pool slot forever. Under load, this silently exhausts the pool. |
| C2 | **No idle connection timeout** | Pooled server connections live forever once checked in | PgBouncer: `server_idle_timeout` (300-600s), `server_lifetime` (3600s) for recycling. ProxySQL: `connection_max_age_ms`, `free_connections_pct` with auto-purge | Stale connections accumulate. After a network blip or PG restart, idle connections become dead sockets that fail on next checkout. No way to detect or clean them up. |
| C3 | **No client-facing TLS** | Proxy responds `'N'` to SSLRequest — always plaintext from client to proxy | PgBouncer: full `client_tls_sslmode` (disable through verify-full), independent of server TLS. HAProxy: `bind *:5432 ssl crt ...` with cipher/protocol control. ProxySQL: `mysql-have_ssl` per-user | SQL traffic (including credentials during auth passthrough) travels in cleartext between client and proxy. Unacceptable if the proxy is network-exposed. |
| C4 | **No TCP keepalive** | No socket options set. Relies on OS defaults (often 2h idle before first probe) | PgBouncer: `tcp_keepalive=1`, `tcp_keepidle=60`, `tcp_keepintvl=10`, `tcp_keepcnt=6`, `tcp_user_timeout=120000`. HAProxy: `option clitcpka`/`option srvtcpka` | Dead connections (client crashes, network partition) are not detected for ~2 hours. Holds pool slots hostage. |
| C5 | **No connect timeout to backend** | `TcpStream::connect(&self.target).await?` in `pool.rs:82` — uses tokio default (no timeout) | PgBouncer: `server_connect_timeout` (5-15s). ProxySQL: `connect_timeout_server` (1s) + `connect_timeout_server_max` (10s). HAProxy: `timeout connect` (3-5s) | If the backend PG is unreachable (firewall, DNS failure), the connect hangs for minutes (TCP SYN retransmit timeout). Client waits the full pool_timeout before getting an error. |

### HIGH — Will cause problems at scale or under adversarial conditions

| # | Gap | What pg-retest Does | What Production Proxies Do | Risk |
|---|-----|---------------------|---------------------------|------|
| H1 | **No message size limit** | `protocol.rs` reads messages of any size into `BytesMut` — length comes from the 4-byte PG header | PgBouncer: `max_packet_size` (default 2GB, configurable). ProxySQL: `max_allowed_packet` (default 4MB). HAProxy: `tune.bufsize` caps per-connection memory | A malicious or buggy client can send a message with length=2GB, causing the proxy to allocate 2GB. A few such connections exhaust memory. |
| H2 | **No per-source connection limit** | Any source IP can open connections up to the global `max_size` | HAProxy: stick tables tracking `conn_cur` and `conn_rate` per source IP, with reject/tarpit actions. ProxySQL: per-user `max_connections`. PgBouncer: `max_user_client_connections`, `max_db_client_connections` | A single misbehaving client can monopolize the entire pool, starving all other clients. |
| H3 | **No auth/login timeout** | Auth passthrough relay has no time bound — waits indefinitely for the server auth exchange to complete | PgBouncer: `client_login_timeout` (30-60s). ProxySQL: `connect_timeout_client` (10s). HAProxy: `timeout client` applies during handshake | Slowloris-style attack: open connections, send partial startup messages, hold connection slots indefinitely without completing auth. |
| H4 | **No TCP_NODELAY** | Default Nagle algorithm active on all sockets | HAProxy uses `TCP_NODELAY` implicitly. PgBouncer: sets `TCP_NODELAY` on all sockets. ProxySQL: worker threads set `TCP_NODELAY` | Small PG protocol messages (Bind, Execute, Sync) are delayed up to 40ms by Nagle's algorithm. Adds measurable latency to prepared-statement workloads. |
| H5 | **No listen backlog tuning** | `TcpListener::bind()` uses tokio/OS default (128 on Linux) | PgBouncer: `listen_backlog` (configurable). HAProxy: `backlog` + kernel `somaxconn` tuning (65535). ProxySQL: `mysql-listen_backlog` | Under burst connection storms, the 128-slot SYN backlog overflows. New connections get RST before the proxy even sees them. Silent connection failures with no log. |
| H6 | **No connection draining** | Graceful shutdown aborts the listener, sleeps 500ms, exits. In-flight queries that take >500ms are killed | HAProxy: `SIGUSR1` → unbind listeners → drain existing → `hard-stop-after` deadline. PgBouncer: `SHUTDOWN WAIT_FOR_CLIENTS` or `PAUSE`/`RESUME`. ProxySQL: `PROXYSQL PAUSE` → drain → restart | 500ms is not enough for long-running analytical queries. No mechanism to stop accepting new work while letting in-flight queries finish naturally. |

### MEDIUM — Operational gaps that matter for production use

| # | Gap | What pg-retest Does | What Production Proxies Do | Risk |
|---|-----|---------------------|---------------------------|------|
| M1 | **No backend health checking** | Pool opens connections on demand; dead backends discovered only when a client query fails | HAProxy: `option pgsql-check user postgres` with `inter 5s fall 3 rise 2`. PgBouncer: detects dead backends on `server_connect_timeout` failure. ProxySQL: 5-thread monitor module with connect/ping/replication checks | If the backend goes down, every new client connection fails one at a time. No circuit breaker — clients keep trying, each eating a connect timeout. |
| M2 | **No connection recycling** | Server connections live until the client disconnects or the proxy shuts down | PgBouncer: `server_lifetime` (3600s) — forces periodic reconnection to pick up DNS changes, rebalance after failover, release PG backend memory | Long-lived connections accumulate PG-side memory (work_mem allocations, temp buffers). No way to rebalance after a failover or topology change. |
| M3 | **No structured proxy metrics** | Control endpoint returns basic JSON status (running, capturing, active_sessions, total_queries, uptime) | HAProxy: built-in Prometheus exporter, per-connection timing, bytes in/out, queue depth, error rates. PgBouncer: `SHOW STATS`/`SHOW POOLS` with per-DB counters. ProxySQL: `stats_mysql_*` tables with per-command latencies | Can't diagnose throughput bottlenecks, connection pool pressure, or latency distribution without metrics. "It's slow" has no data to back it up. |
| M4 | **No configurable buffer sizes** | Uses tokio default `BufReader`/`BufWriter` (8KB each) | HAProxy: `tune.bufsize` (default 16KB, tunable). PgBouncer: `pkt_buf` (4KB, tunable). ProxySQL: `max_allowed_packet` threshold tuning | 8KB is fine for OLTP but undersized for large result sets or bulk INSERTs. Can't tune memory-per-connection tradeoff. |
| M5 | **No idle-in-transaction timeout** | No detection of sessions stuck in an open transaction | PgBouncer: `idle_transaction_timeout` (60s recommended). ProxySQL: `max_transaction_idle_time` (4h default) | A client that opens a transaction and then crashes (or leaks a connection) holds a server connection with an open transaction indefinitely. PG holds row locks, prevents VACUUM, and the connection can't be reused. |
| M6 | **Unbounded prepared statement cache** | Per-connection `HashMap<String, String>` (statement name → SQL) with no eviction | ProxySQL: manages prepared statement lifecycle with multiplexing awareness. PgBouncer: tracks statements in transaction mode | Extreme edge case, but a client that creates millions of unique prepared statements would grow memory without bound. |

### LOW — Nice-to-have hardening

| # | Gap | What pg-retest Does | What Production Proxies Do | Risk |
|---|-----|---------------------|---------------------------|------|
| L1 | **No `TCP_DEFER_ACCEPT`** | Accept returns immediately on SYN-ACK | PgBouncer: `tcp_defer_accept=1`. HAProxy: implicit | Minor: kernel accepts connections before data arrives, wasting a wake-up. Marginal under high connection rates. |
| L2 | **No `SO_REUSEPORT`** | Single listener, single accept loop | PgBouncer: `so_reuseport=1` for multi-instance. HAProxy: `nbthread` + per-thread `epoll` | Not needed for tokio (multi-threaded runtime distributes work across cores). Only relevant if accept() becomes a bottleneck at extreme rates. |
| L3 | **No zero-copy forwarding** | Data copies through userspace buffers (BufReader → BytesMut → BufWriter) | HAProxy: `option splice-auto` (Linux `splice()` syscall, zero-copy kernel pipe) | Minor throughput/CPU overhead. Only matters at very high data rates. Also incompatible with capture (must inspect data). |
| L4 | **No TLS session caching** | Each TLS connection to backend does full handshake | HAProxy: `tune.ssl.cachesize` (20K entries), session tickets. PgBouncer: relies on OpenSSL session cache | Extra RTT per new backend connection. Marginal impact since pool reuses connections. |
| L5 | **No retry logic for backend connections** | Single connect attempt; failure = error to client | HAProxy: configurable retries with `retries 3`. ProxySQL: `connect_retries_on_failure` (10 retries, 1ms delay) | Transient connect failures (port exhaustion, brief PG restart) aren't retried. |
| L6 | **No `application_name` tracking** | Proxy doesn't inject identifying info into upstream connections | PgBouncer: `application_name_add_host=1` appends client IP to `application_name` in `pg_stat_activity` | Hard to trace which proxy connection maps to which original client when debugging via PG-side tools. |

---

## Prioritized Remediation Plan

### Phase 1: Stop the Bleeding (prevents hung connections and resource exhaustion)

These are the highest-impact, lowest-effort changes. Most are a few lines of code.

| Item | Change | Effort | Files |
|------|--------|--------|-------|
| **C1** | Wrap relay read loops in `tokio::time::timeout()`. Add `--client-timeout` (default 300s) and `--server-timeout` (default 300s) CLI flags. | Small | `connection.rs` |
| **C4** | Set `TCP_KEEPALIVE`, `TCP_KEEPIDLE` (60s), `TCP_KEEPINTVL` (10s), `TCP_KEEPCNT` (6) on both client and server sockets using `socket2` crate. | Small | `connection.rs`, `pool.rs` |
| **C5** | Wrap `TcpStream::connect()` in `tokio::time::timeout()`. Add `--connect-timeout` (default 5s). | Trivial | `pool.rs` |
| **H4** | Call `stream.set_nodelay(true)` on both client and server `TcpStream`. | Trivial | `connection.rs`, `pool.rs` |
| **H5** | Use `socket2::Socket` to create the listener with a configurable backlog (default 1024). Add `--listen-backlog`. | Small | `listener.rs`, `cli.rs` |

### Phase 2: Resource Protection (prevents abuse and starvation)

| Item | Change | Effort | Files |
|------|--------|--------|-------|
| **H1** | Add max message size check in `protocol.rs` after reading the 4-byte length. Default 64MB, configurable via `--max-message-size`. Reject with PG ErrorResponse. | Small | `protocol.rs` |
| **H2** | Add per-source-IP connection counter (`DashMap<IpAddr, AtomicU32>`). Reject with PG ErrorResponse when limit exceeded. Add `--max-connections-per-ip` (default 0 = unlimited). | Medium | `listener.rs`, `mod.rs` |
| **H3** | Wrap the auth passthrough relay in `tokio::time::timeout()`. Default 30s. Add `--auth-timeout`. | Small | `connection.rs` |
| **C2** | Add idle reaper task: periodically scan pool idle queue, close connections older than `--server-idle-timeout` (default 600s). Track creation time in `ServerConn`. | Medium | `pool.rs` |
| **M5** | Track last-activity timestamp per connection. If in-transaction and idle > `--idle-transaction-timeout` (default 0 = disabled), send CancelRequest and close. | Medium | `connection.rs`, `pool.rs` |

### Phase 3: Operational Maturity (production observability and resilience)

| Item | Change | Effort | Files |
|------|--------|--------|-------|
| **C3** | Add client-facing TLS via rustls `TlsAcceptor`. Reuse existing `TlsMode` enum. Add `--client-tls-cert` and `--client-tls-key` flags. When set, accept `SSLRequest` with `'S'` and upgrade. | Medium | `connection.rs`, `tls.rs`, `cli.rs` |
| **H6** | Replace 500ms sleep with drain-then-deadline: stop listener → wait for active count to reach 0 → `--shutdown-timeout` (default 30s) hard deadline. | Medium | `mod.rs` |
| **M1** | Add background health check task: periodically send a simple query (`SELECT 1`) on an idle pool connection. If it fails N times, mark pool degraded and log a warning. | Medium | `pool.rs`, `mod.rs` |
| **M2** | Track connection birth time in `ServerConn`. During checkin, if age > `--server-lifetime` (default 3600s), discard instead of returning to pool. | Small | `pool.rs` |
| **M3** | Add a `/metrics` endpoint to the control server with: connections (active/idle/waiting/total), queries/sec, bytes in/out, connect latency histogram, pool utilization. | Medium-Large | `control.rs`, new `metrics.rs` |
| **L6** | During startup relay, inject `application_name=pg-retest-proxy-{client_ip}` into the startup parameters. Add `--track-client-ip` flag. | Small | `connection.rs`, `protocol.rs` |

---

## What We Don't Need (and Why)

Not everything production proxies do is relevant to pg-retest. These are conscious non-gaps:

| Feature | Why We Skip It |
|---------|---------------|
| **Connection multiplexing** (ProxySQL-style) | pg-retest is session-mode by design — it must preserve per-connection SQL streams for accurate capture and replay. Multiplexing would break capture fidelity. |
| **Query routing / read-write splitting** | Not a load balancer. Single upstream target by design. |
| **OCSP stapling** | Internal/dev-adjacent tool, not internet-facing. |
| **Zero-copy / splice** | Incompatible with capture — we must read and inspect every message in userspace. |
| **Multi-process (`SO_REUSEPORT`)** | Tokio's multi-threaded runtime already distributes work across cores. Single-process is simpler and sufficient. |
| **Proxy-level authentication** | Auth passthrough to PG is the right design — the proxy shouldn't maintain its own credential store for database users. |
| **Query caching** | Not a caching proxy. Every query must reach the backend for accurate timing. |
| **Connection warming / `min_pool_size`** | Useful for poolers serving web apps, but pg-retest connections are driven by captured workload patterns, not steady-state traffic. |

---

## Reference: Source File Map

| File | Lines | Role |
|------|-------|------|
| `src/proxy/mod.rs` | 441 | Proxy orchestration (CLI, persistent, web modes) |
| `src/proxy/listener.rs` | 61 | TCP accept loop |
| `src/proxy/connection.rs` | 713 | Per-connection handling, auth relay, bidirectional relay |
| `src/proxy/pool.rs` | 135 | Session-mode connection pool |
| `src/proxy/protocol.rs` | 762 | PG protocol v3 message parsing |
| `src/proxy/capture.rs` | 998 | Capture event collection and profile building |
| `src/proxy/control.rs` | 333 | HTTP control endpoint |
| `src/proxy/staging.rs` | 321 | SQLite staging for capture data |
| `src/tls.rs` | 91 | TLS connector (upstream only) |
