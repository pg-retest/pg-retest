# Proxy/Network Layer Best Practices Research

**Date:** 2026-03-27
**Purpose:** Compare HAProxy, ProxySQL, and PgBouncer proxy layer implementation patterns against pg-retest's custom PostgreSQL proxy to identify gaps.

---

## 1. HAProxy

### 1.1 Connection Management

**maxconn Hierarchy** (4 levels, most specific wins):

| Level | Scope | Default | Production Recommendation |
|-------|-------|---------|--------------------------|
| `global maxconn` | Process-wide ceiling | 2000 | 50,000-100,000+ |
| `defaults maxconn` | All listeners unless overridden | inherits global | -- |
| `frontend/listen maxconn` | Per-listener limit | inherits defaults | Size per service |
| `server maxconn` | Per-backend-server limit | 0 (unlimited) | 64-256 per server |

**Backlog:**
- `backlog <N>` on frontend/listen: TCP listen queue size (passed to `listen(2)`)
- Production: match or exceed `net.core.somaxconn` (set kernel to 65535)
- Also set `net.ipv4.tcp_max_syn_backlog = 65535`

**Queue Management:**
- `maxqueue <N>` on server lines: max pending connections per backend server (e.g., 128)
- `timeout queue <ms>`: max time a connection waits in queue before 503 (e.g., 5000ms)
- `balance leastconn`: route to server with fewest active connections (ideal for DB proxying)
- `fullconn <N>`: threshold at which HAProxy switches from static to dynamic connection distribution algorithms

### 1.2 Timeout Configuration

| Parameter | Default | Production Rec | Purpose |
|-----------|---------|---------------|---------|
| `timeout connect` | none (must set) | 3000-5000ms | TCP connect to backend |
| `timeout client` | none (must set) | 30000-50000ms | Client-side inactivity |
| `timeout server` | none (must set) | 30000-50000ms | Server-side inactivity |
| `timeout tunnel` | none | 3600s | Bidirectional idle (for long-lived DB connections) |
| `timeout http-keep-alive` | none | 5000-10000ms | Idle between HTTP requests on keep-alive |
| `timeout check` | none | 3500ms | Health check response window |
| `timeout queue` | `timeout connect` | 5000ms | Queued connection wait |
| `hard-stop-after` | none | 30s-60s | Force shutdown deadline after graceful stop begins |

**Key insight for DB proxying:** `timeout tunnel` is critical for long-lived database connections that may be idle between queries. Without it, `timeout client`/`timeout server` will kill idle DB sessions.

### 1.3 Buffer Management

| Parameter | Default | Production Notes |
|-----------|---------|-----------------|
| `tune.bufsize` | 16384 (16KB) | Memory per connection = 2x bufsize. Increase for large queries/results. Each doubling halves max concurrent connections for same RAM. |
| `tune.maxrewrite` | ~1024 | Reserved header rewrite space. First socket reads fill at most `bufsize - maxrewrite`. Auto-capped to half of bufsize. |
| `tune.pipesize` | varies | Kernel pipe buffer for splice(). Larger = fewer syscalls during zero-copy. |
| `tune.rcvbuf.client` | OS default | SO_RCVBUF for client sockets |
| `tune.rcvbuf.server` | OS default | SO_RCVBUF for server sockets |
| `tune.sndbuf.client` | OS default | SO_SNDBUF for client sockets |
| `tune.sndbuf.server` | OS default | SO_SNDBUF for server sockets |

### 1.4 Health Checking

**Basic TCP check:**
```
server db1 10.0.0.1:5432 check inter 5s fall 3 rise 2
```

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `inter` | 2s | Check interval |
| `fall` | 3 | Consecutive failures before marking DOWN |
| `rise` | 2 | Consecutive successes before marking UP |
| `fastinter` | `inter` | Interval while transitioning (faster detection) |
| `downinter` | `inter` | Interval while DOWN (can be slower to reduce load) |

**Protocol-specific checks:**
- `option pgsql-check user <user>` — PostgreSQL protocol-level check (sends StartupMessage)
- `option mysql-check user <user>` — MySQL protocol-level check
- `option httpchk GET /health` — HTTP health endpoint

**Agent checks** (external health agent):
- `agent-check agent-port <port> agent-inter <interval>`
- Agent returns: `UP`, `DOWN`, `DRAIN`, `MAINT`, or percentage (0-100%) for weight adjustment

### 1.5 Graceful Shutdown / Drain

| Mechanism | Signal/Command | Behavior |
|-----------|---------------|----------|
| Hard stop | `SIGTERM` | Immediate exit, all connections dropped |
| Graceful stop | `SIGUSR1` | Unbind listeners, drain existing connections, exit when last closes |
| `hard-stop-after <duration>` | (config) | Force kill after duration if graceful hasn't completed |
| Server drain | `set server <backend>/<server> state drain` (via socat) | Stop new connections, finish existing |
| Server maintenance | `set server <backend>/<server> state maint` | Immediately remove from rotation |

**Seamless reload (modern HAProxy):**
- `systemctl reload haproxy` or sending `SIGUSR2`
- New process inherits listening sockets from old process via `fd@` transfer
- Old process enters graceful drain mode
- Zero dropped connections during config reload

### 1.6 TLS Termination

**Global defaults (production):**
```
global
    ssl-default-bind-options no-sslv3 no-tlsv10 no-tlsv11 prefer-client-ciphers
    ssl-default-bind-ciphersuites TLS_AES_256_GCM_SHA384:TLS_CHACHA20_POLY1305_SHA256:TLS_AES_128_GCM_SHA256
    ssl-default-bind-ciphers ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384:ECDHE-ECDSA-CHACHA20-POLY1305:ECDHE-RSA-CHACHA20-POLY1305
    ssl-dh-param-file /etc/haproxy/dhparam.pem
    tune.ssl.default-dh-param 2048
    tune.ssl.cachesize 20000
    tune.ssl.maxrecord 1460
```

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `tune.ssl.default-dh-param` | 1024 | DH param size. Must be >= 2048 for production. |
| `tune.ssl.cachesize` | 20000 | SSL session cache entries (session resumption) |
| `tune.ssl.maxrecord` | 16384 | Max TLS record size. Set to ~1460 for low-latency first byte (fits in single TCP segment). |
| `tune.ssl.lifetime` | 300s | SSL session cache lifetime |

**OCSP stapling:** HAProxy fetches and caches OCSP responses, including them in TLS handshake. Eliminates client-side OCSP lookup latency. Configured per certificate with `ocsp-update on`.

**Frontend/backend TLS separation:** HAProxy can terminate TLS on frontend (`bind *:5432 ssl crt /path/cert.pem`) and re-encrypt to backend (`server db1 10.0.0.1:5432 ssl verify required ca-file /path/ca.pem`).

### 1.7 TCP Keepalive

| Parameter | Scope | Purpose |
|-----------|-------|---------|
| `option tcpka` | both sides | Enable TCP keepalive on all connections |
| `option clitcpka` | client-side | Enable TCP keepalive on client connections only |
| `option srvtcpka` | server-side | Enable TCP keepalive on server connections only |

These use OS-level keepalive settings (`tcp_keepalive_time`, `tcp_keepalive_intvl`, `tcp_keepalive_probes`). HAProxy does not set per-socket keepalive intervals; tune via sysctl:
```
net.ipv4.tcp_keepalive_time = 60
net.ipv4.tcp_keepalive_intvl = 10
net.ipv4.tcp_keepalive_probes = 6
```

### 1.8 Rate Limiting / Stick Tables

**Stick table definition:**
```
backend rate_limit
    stick-table type ip size 200k expire 3m store conn_cur,conn_rate(3m),http_req_rate(3m),http_err_rate(3m),gpc0,gpc1
```

**Data types storable per entry:**

| Counter | Purpose |
|---------|---------|
| `conn_cur` | Current concurrent connections |
| `conn_rate(<period>)` | Connection rate over period |
| `conn_cnt` | Total connection count |
| `http_req_rate(<period>)` | HTTP request rate |
| `http_req_cnt` | Total HTTP request count |
| `http_err_rate(<period>)` | HTTP error rate |
| `http_err_cnt` | Total HTTP error count |
| `bytes_in_rate(<period>)` | Incoming bandwidth rate |
| `bytes_out_rate(<period>)` | Outgoing bandwidth rate |
| `gpc0`, `gpc1` | General purpose counters (custom logic) |
| `gpc0_rate(<period>)` | Rate of gpc0 increments |

**Tracking and enforcement:**
```
frontend pg_proxy
    bind *:5432
    tcp-request connection track-sc0 src table rate_limit
    tcp-request connection reject if { sc_conn_rate(0) gt 100 }
    tcp-request connection reject if { sc_conn_cur(0) gt 20 }
```

**Actions:** `reject`, `tarpit` (hold connection open consuming attacker resources), `silent-drop`, `set-var`, `sc-inc-gpc0`

**Sticky counters:** `sc0`, `sc1`, `sc2` (up to 3 independent tracking contexts per connection, e.g., track by source IP in sc0 and by authenticated user in sc1).

### 1.9 Connection Limits Per Source

Via stick tables (see above) — track `conn_cur` and `conn_rate` per `src` IP, then enforce ACLs:
```
tcp-request connection reject if { src_conn_cur ge 50 }
tcp-request connection reject if { src_conn_rate(10s) ge 100 }
```

### 1.10 Logging and Observability

- `log <address> <facility> <level>` — syslog integration
- Per-connection log format: client IP, timers (Tq/Tw/Tc/Tr/Tt), status, bytes, backend, server, retries, queue
- Runtime stats socket: `stats socket /var/run/haproxy.sock mode 600 level admin`
- Prometheus exporter: built-in via `http-request use-service prometheus-exporter`
- `option tcplog` — detailed TCP connection logging with timing breakdown
- `option dontlognull` — suppress logging for connections that send no data (probe cleanup)

### 1.11 Zero-Copy / Splice

| Parameter | Purpose |
|-----------|---------|
| `option splice-auto` | Automatically use kernel splice() when both sides are sockets (zero-copy) |
| `option splice-request` | Splice client-to-server direction only |
| `option splice-response` | Splice server-to-client direction only |
| `no splice` | Disable splice, use conventional recv/send |

**How it works:** Linux `splice()` syscall moves data between file descriptors via kernel pipe without copying to/from userspace. Reduces CPU and memory bandwidth. Falls back to standard copy when splice isn't possible (e.g., TLS connections where data must be decrypted in userspace).

**Tuning:** `tune.pipesize` controls kernel pipe buffer size for splice operations.

### 1.12 Threading

| Parameter | Purpose |
|-----------|---------|
| `nbthread <N>` | Worker threads (upper limit = CPU count) |
| `cpu-map auto:1/1-N 0-N` | Pin threads to CPU cores |

HAProxy 2.x+ uses a multi-threaded event loop model (not thread-per-connection). All threads share the same memory, with lock-free data structures where possible. Each thread runs its own event loop via `epoll`.

---

## 2. ProxySQL

### 2.1 Connection Multiplexing

ProxySQL multiplexes frontend (client) connections onto a shared pool of backend connections. A single backend connection can serve multiple frontend connections sequentially.

**When a backend connection returns to pool:**
- Query completes AND no active transaction AND no session variables modified AND no locks held AND autocommit is ON
- Connection is in a "clean" state

**Conditions that DISABLE multiplexing (connection pinned to client):**

| Condition | Duration | Recoverable? |
|-----------|----------|--------------|
| Active transaction | Until COMMIT/ROLLBACK | Yes |
| `LOCK TABLE` / `FLUSH TABLES WITH READ LOCK` | Until released | Yes |
| Session/user variables (`@var`) | Permanent | No (until disconnect) |
| `SQL_CALC_FOUND_ROWS` | Permanent | No |
| `CREATE TEMPORARY TABLE` | Permanent | No |
| `PREPARE` (text protocol) | Permanent | No |
| `SQL_LOG_BIN=0` | Until reset to 1 | Yes |
| Error on connection (`last_errno != 0`) | Permanent | No |
| `autocommit=OFF` (when `mysql-autocommit_false_is_transaction=true`) | Until autocommit ON | Yes |

**Tuning parameters:**
- `mysql-auto_increment_delay_multiplex` (default: 5) — delay multiplexing N queries after LAST_INSERT_ID()
- `mysql-connection_delay_multiplex_ms` (default: 0) — delay multiplexing re-enable by N ms
- `mysql_query_rules.multiplexing` — per-rule override (0=disable, 1=enable, 2=force-enable for `@` queries)

**Production outcome:** 10,000 client connections sustained by 100-200 backend connections is realistic.

### 2.2 Connection Pool Management

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `mysql-max_connections` | 2048 | Global max client connections (range: 1-1,000,000) |
| `mysql-free_connections_pct` | 10% | % of `max_connections` per server kept as idle pool |
| `mysql-connection_max_age_ms` | 0 (disabled) | Max lifetime for pooled connections |
| `mysql-connection_warming` | false | Pre-warm connections based on `free_connections_pct` |
| `mysql-connpoll_reset_queue_length` | 50 | Reset queue threshold; destroy connections when exceeded |
| `mysql-max_stale_connections` | -- | (via `max_connections` per server in `mysql_servers`) |
| `mysql-session_idle_ms` | 1ms | Idle session detection interval |

**Per-server pool:** In `mysql_servers` table, `max_connections` per server controls the ceiling. Free pool per server = `free_connections_pct * server.max_connections / 100`.

**Connection lifecycle:**
1. Client connects to ProxySQL frontend
2. Query arrives, ProxySQL checks pool for clean backend connection
3. If none available, opens new connection (up to server `max_connections`)
4. Query executes, result returned to client
5. If connection clean (no tx, no session vars), returned to pool
6. If connection dirty, remains pinned to client

### 2.3 Query Timeout Handling

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `mysql-default_query_timeout` | 86,400,000ms (24h) | Global query timeout |
| `mysql-long_query_time` | 1000ms | Threshold for slow query stats |
| `mysql-max_transaction_idle_time` | 14,400,000ms (4h) | Kill idle-in-transaction connections |
| `mysql-max_transaction_time` | 14,400,000ms (4h) | Kill long-running transactions |
| `mysql-connect_timeout_server` | 1000ms | Single connection attempt timeout |
| `mysql-connect_timeout_server_max` | 10,000ms | Cumulative connection timeout |
| `mysql-connect_timeout_client` | 10,000ms | Client handshake timeout |
| `mysql-ping_timeout_server` | 200ms | Backend keepalive ping timeout |
| `mysql-ping_interval_server_msec` | 10,000ms | Backend ping interval |

**Kill mechanism:** When query timeout fires, ProxySQL spawns a separate thread, opens a new backend connection, and issues `KILL QUERY <id>` to terminate the running query. Returns error to client.

**Retry logic:**
- `mysql-connect_retries_on_failure` (default: 10) — retry backend connections
- `mysql-connect_retries_delay` (default: 1ms) — delay between retries
- `mysql-query_retries_on_failure` (default: 1) — auto-retry failed queries (only for idempotent/read queries)

### 2.4 Thread Model

| Thread Type | Count | Purpose |
|-------------|-------|---------|
| MySQL Workers | `mysql-threads` (default: 4) | Handle all client/backend MySQL traffic |
| Idle Threads | `--idle-threads` startup flag | Manage idle connections (offload from workers) |
| Admin Thread | 1 | Admin interface, config, clustering |
| Monitor Threads | 5+ (auto-scaling pool) | Health checks (connect, ping, read_only, repl_lag) |
| Query Cache Purge | 1 | Background GC for query cache |
| Cluster Threads | 1 per node | Cluster sync |

**Key design:** Once a client connection is assigned to a worker thread, it stays on that thread for its lifetime. Worker threads use `poll()` for I/O multiplexing.

**Scaling:** `--idle-threads` is critical for high connection counts (tested to 1M connections). Idle connections consume minimal worker resources.

**Stack size:** `mysql-stacksize` (default: 1MB per thread)

### 2.5 Buffer and Packet Size

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `mysql-max_allowed_packet` | 4MB (4,194,304) | Max single packet from client (range: 8KB-1GB) |
| `mysql-threshold_query_length` | 524,288 bytes | Large query handling threshold |
| `mysql-threshold_resultset_size` | 4,194,304 bytes | Large resultset handling threshold |
| `mysql-poll_timeout` | 2000ms | I/O poll() timeout |
| `mysql-poll_timeout_on_failure` | 100ms | Reduced timeout after errors |

### 2.6 Health Check (Monitor Module)

5 dedicated threads, auto-scaling worker pool:

| Check Type | Interval Variable | Timeout Variable | Purpose |
|------------|-------------------|------------------|---------|
| Connect | `mysql-monitor_connect_interval` | `mysql-monitor_connect_timeout` | Verify TCP connectivity |
| Ping | `mysql-monitor_ping_interval` | `mysql-monitor_ping_timeout` | Application-level liveness |
| Read Only | `mysql-monitor_read_only_interval` | `mysql-monitor_read_only_timeout` | Detect primary/replica role |
| Replication Lag | `mysql-monitor_replication_lag_interval` | `mysql-monitor_replication_lag_timeout` | Seconds_Behind_Master tracking |
| Group Replication | (configurable) | (configurable) | InnoDB cluster state |

**Failure handling:**
- `mysql-monitor_ping_max_failures` — consecutive failures before marking node unreachable
- Unhealthy nodes: shunned (repl lag), removed from hostgroup (read_only change), all connections killed (ping failures)
- Connection auto-purge: connections alive > 3x `monitor_ping_interval` are purged

### 2.7 Graceful Shutdown

| Command | Behavior |
|---------|----------|
| `PROXYSQL SHUTDOWN` | Graceful module-by-module shutdown |
| `PROXYSQL KILL` | Immediate process kill |
| `PROXYSQL PAUSE` | Stop accepting new connections; existing connections continue. Enables rolling restart when paired with a second proxy. |
| `PROXYSQL RESUME` | Resume accepting connections after PAUSE |

**Rolling restart pattern:**
1. `PROXYSQL PAUSE` on instance A
2. All new connections go to instance B
3. Wait for instance A connections to drain
4. Restart instance A
5. `PROXYSQL RESUME` on instance A
6. Repeat for instance B

### 2.8 TLS Frontend/Backend Separation

**Frontend (client-to-proxy):**
- `mysql-have_ssl` (default: true since v2.6.0) — enable TLS for client connections
- Per-user `use_ssl=1` in `mysql_users` table

**Backend (proxy-to-server):**
- `mysql-ssl_p2s_ca` — CA certificate file
- `mysql-ssl_p2s_cert` — Client certificate
- `mysql-ssl_p2s_key` — Client private key
- `mysql-ssl_p2s_cipher` — Cipher list
- `mysql-ssl_p2s_crl` / `mysql-ssl_p2s_crlpath` — CRL files
- Per-server `use_ssl=1` in `mysql_servers` table

Frontend and backend TLS are completely independent — you can have TLS on frontend only, backend only, both, or neither.

### 2.9 Rate Limiting / Throttling

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `mysql-default_query_delay` | 0ms | Global delay added to every query |
| `mysql-throttle_connections_per_sec_to_hostgroup` | 1,000,000 | Max new connections/sec to hostgroup |
| `mysql-throttle_max_bytes_per_second_to_client` | 0 (unlimited) | Bandwidth limit to clients |
| `mysql-throttle_ratio_server_to_client` | 0 (disabled) | Server-to-client bandwidth ratio |

**Per-rule throttling:** `mysql_query_rules.delay` field adds per-millisecond delay to matching queries. Enables surgical throttling of specific query patterns.

### 2.10 Max Packet Size Enforcement

`mysql-max_allowed_packet` (default: 4MB, max: 1GB) — queries exceeding this are rejected. Mimics MySQL's `max_allowed_packet`. Set slightly larger than backend MySQL's setting to avoid silent truncation.

### 2.11 Connection Backlog

`mysql-listen_backlog` — pending TCP connection queue (defaults to OS TCP backlog). On Linux, bounded by `net.core.somaxconn`.

---

## 3. PgBouncer

### 3.1 Connection Pooling Modes

| Mode | When server connection released | Limitations |
|------|-------------------------------|-------------|
| **session** | When client disconnects | None — full PG feature support |
| **transaction** | After COMMIT/ROLLBACK | No session-level features (LISTEN/NOTIFY, prepared statements, SET, advisory locks) |
| **statement** | After each statement | Single-statement transactions only, no multi-statement tx, no SET |

**Production recommendation:** Transaction mode for most web/API workloads (best connection reduction). Session mode only when application requires session-level state.

### 3.2 Max Client Connections vs Server Connections

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `max_client_conn` | 100 | Total client connections PgBouncer will accept |
| `max_db_connections` | 0 (unlimited) | Server connections per database |
| `max_db_client_connections` | 0 (unlimited) | Client connections per database |
| `max_user_connections` | 0 (unlimited) | Server connections per user |
| `max_user_client_connections` | 0 (unlimited) | Client connections per user |

**Production sizing example (10K connections):**
```ini
max_client_conn = 10000
max_db_connections = 100        # actual PG connections per DB
max_user_connections = 100      # actual PG connections per user
```

**File descriptor math:** total FDs needed = `max_client_conn + (max_pool_size * total_databases * total_users)`. Adjust `ulimit -n` accordingly.

### 3.3 Reserve Pool and Pool Sizing

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `default_pool_size` | 20 | Server connections per user/database pair |
| `min_pool_size` | 0 | Minimum connections maintained (pre-warming) |
| `reserve_pool_size` | 0 | Extra connections for burst handling |
| `reserve_pool_timeout` | 5.0s | Wait time before tapping reserve pool |

**How reserve pool works:** If a client has been waiting longer than `reserve_pool_timeout` for a server connection, PgBouncer opens additional connections up to `reserve_pool_size` beyond `default_pool_size`. These extra connections are released when load drops.

**Production sizing:**
```ini
default_pool_size = 25          # normal capacity
min_pool_size = 10              # always warm
reserve_pool_size = 10          # burst headroom
reserve_pool_timeout = 5        # trigger threshold
```

### 3.4 Timeout Configuration

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `server_connect_timeout` | 15.0s | TCP + auth to backend PG |
| `server_idle_timeout` | 600.0s | Close idle server connections |
| `server_lifetime` | 3600.0s | Max lifetime for server connections (recycling) |
| `server_login_retry` | 15.0s | Retry delay after failed backend login |
| `client_idle_timeout` | 0.0s (disabled) | Close idle client connections |
| `client_login_timeout` | 60.0s | Auth timeout for new clients |
| `query_timeout` | 0.0s (disabled) | Cancel queries exceeding duration |
| `query_wait_timeout` | 120.0s | Max wait for server connection assignment |
| `cancel_wait_timeout` | 10.0s | Max wait for cancel request processing |
| `idle_transaction_timeout` | 0.0s (disabled) | Kill idle-in-transaction sessions |
| `transaction_timeout` | 0.0s (disabled) | Kill long-running transactions |
| `suspend_timeout` | 10s | Buffer flush deadline during SUSPEND/reboot |

**Production recommendations:**
```ini
server_connect_timeout = 5       # fail fast
server_idle_timeout = 300        # release idle connections sooner
server_lifetime = 3600           # recycle hourly
client_login_timeout = 30        # tight auth window
query_wait_timeout = 30          # don't queue indefinitely
idle_transaction_timeout = 60    # prevent connection hoarding
```

### 3.5 TLS Configuration (Client and Server Independent)

**Client-side (PgBouncer as TLS server):**

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `client_tls_sslmode` | disable | Options: disable, allow, prefer, require, verify-ca, verify-full |
| `client_tls_key_file` | -- | Private key for PgBouncer |
| `client_tls_cert_file` | -- | Certificate for PgBouncer |
| `client_tls_ca_file` | -- | CA for client cert validation |
| `client_tls_protocols` | secure | tlsv1.0, tlsv1.1, tlsv1.2, tlsv1.3, all, secure |
| `client_tls_ciphers` | default | OpenSSL cipher string (TLS 1.2 and below) |
| `client_tls13_ciphers` | (empty) | TLS 1.3 cipher suites |
| `client_tls_ecdhcurve` | auto | ECDH curve name |
| `client_tls_dheparams` | auto | DHE param size (none, auto=2048, legacy=1024) |

**Server-side (PgBouncer as TLS client):**

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `server_tls_sslmode` | prefer | Same options as client_tls_sslmode |
| `server_tls_ca_file` | -- | CA to verify PG server cert |
| `server_tls_key_file` | -- | Client cert key (mutual TLS) |
| `server_tls_cert_file` | -- | Client certificate |
| `server_tls_protocols` | secure | Protocol version control |
| `server_tls_ciphers` | default | OpenSSL cipher string |
| `server_tls13_ciphers` | (empty) | TLS 1.3 ciphers |

**Key design:** Frontend and backend TLS are fully independent. You can terminate TLS on the client side while connecting to PG in plaintext, or vice versa.

### 3.6 Authentication Passthrough

| Parameter | Purpose |
|-----------|---------|
| `auth_type` | Authentication method: trust, any, password, md5, scram-sha-256, cert, hba, pam, peer |
| `auth_file` | userlist.txt with passwords (plain, md5, or SCRAM secrets) |
| `auth_user` | PG user that PgBouncer uses to run `auth_query` |
| `auth_query` | SQL query to fetch password hash from PG (e.g., `SELECT usename, passwd FROM pg_shadow WHERE usename=$1`) |
| `auth_hba_file` | pg_hba.conf-style rules for HBA auth |
| `auth_dbname` | Database used for `auth_query` |

**SCRAM passthrough:** Since PgBouncer 1.21+, SCRAM-SHA-256 authentication can pass through — PgBouncer stores SCRAM verifiers and performs the SCRAM exchange with the client, then authenticates to PG separately. No plaintext passwords needed.

### 3.7 DNS Resolution

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `dns_max_ttl` | 15.0s | DNS cache lifetime |
| `dns_nxdomain_ttl` | 15.0s | Negative DNS cache lifetime |
| `dns_zone_check_period` | 0 (disabled) | SOA-based zone change detection (requires c-ares) |

**Design:** Short DNS TTLs (default 15s) enable quick failover when backend IPs change. `dns_zone_check_period` provides event-driven DNS refresh instead of polling.

### 3.8 Graceful Restart (Online Restart)

**Classic method (pre-1.21):**
1. `SUSPEND` — flush all buffers, stop processing
2. Start new PgBouncer process with `-R` flag (inherits FDs via Unix socket)
3. New process picks up existing connections
4. Old process exits

**Modern method (1.21+ with SO_REUSEPORT):**
1. Run multiple PgBouncer instances on same port (`so_reuseport = 1`)
2. `SHUTDOWN WAIT_FOR_CLIENTS` on target instance
3. Instance stops accepting new connections
4. Kernel routes new connections to remaining instances
5. Instance exits when last client disconnects
6. Restart instance
7. Repeat for next instance

### 3.9 SUSPEND/RESUME for Zero-Downtime

| Command | Behavior |
|---------|----------|
| `SUSPEND` | Flush all socket buffers, stop accepting data on all sockets. Process sleeps. |
| `RESUME` | Wake up, resume normal processing. |
| `PAUSE [db]` | Disconnect from servers (per pooling mode). New queries wait. |
| `RESUME [db]` | Reconnect and resume after PAUSE. |

**SUSPEND vs PAUSE:** SUSPEND is lower-level (socket buffer flush for process-level operations like online restart). PAUSE is higher-level (disconnect from backends while keeping client connections alive).

### 3.10 Logging / Stats

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `log_connections` | 1 | Log client connections |
| `log_disconnections` | 1 | Log client disconnections with reasons |
| `log_pooler_errors` | 1 | Log errors sent to clients |
| `log_stats` | 1 | Write periodic stats to log |
| `stats_period` | 60s | Stats logging interval |
| `verbose` | 0 | Verbosity (0-3) |

**SHOW commands for observability:**
- `SHOW STATS` — per-DB transaction/query/byte counters
- `SHOW POOLS` — pool state (cl_active, cl_waiting, sv_active, sv_idle, sv_used, sv_login, maxwait)
- `SHOW SERVERS` / `SHOW CLIENTS` — per-connection details
- `SHOW MEM` — memory allocation breakdown
- `SHOW LISTS` — internal resource counters (databases, users, pools, free_clients, used_clients, etc.)

### 3.11 SO_REUSEPORT for Multi-Process

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `so_reuseport` | 0 | Enable SO_REUSEPORT on listening sockets |
| `listen_backlog` | 128 | TCP listen queue depth |
| `tcp_defer_accept` | 1 (Linux) | Defer accept() until data arrives |

**Multi-process scaling:** PgBouncer is single-threaded (event-loop based). `so_reuseport` allows multiple instances on the same port, with kernel load-balancing connections across them. Each instance needs separate `unix_socket_dir`, `pidfile`, and `logfile`.

**Production scaling:** Run N instances (N = CPU cores) with `so_reuseport = 1` for multi-core utilization.

### 3.12 TCP Keepalive

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `tcp_keepalive` | 1 (enabled) | Enable basic OS-level TCP keepalive |
| `tcp_keepidle` | OS default | Seconds before first keepalive probe |
| `tcp_keepintvl` | OS default | Seconds between keepalive probes |
| `tcp_keepcnt` | OS default | Failed probes before connection declared dead |
| `tcp_user_timeout` | 0 (OS default) | Max ms for unacked data (Linux only; overrides keepalive for detection speed) |

**Production recommendations:**
```ini
tcp_keepalive = 1
tcp_keepidle = 60
tcp_keepintvl = 10
tcp_keepcnt = 6
tcp_user_timeout = 120000       # 2 minutes
```

`tcp_user_timeout` is the most impactful for fast dead-connection detection — it bounds the total time TCP will wait for ACKs regardless of keepalive settings.

### 3.13 Application Name Tracking

`application_name_add_host` (default: 0) — appends `client_host:port` to `application_name` sent to PG. Invaluable for tracing which pooled connection maps to which original client in `pg_stat_activity`.

### 3.14 Server Lifetime / Connection Recycling

`server_lifetime` (default: 3600s = 1 hour) — close server connections that have been alive longer than this, even if still working. Forces periodic reconnection to pick up DNS changes, rebalance after failover, and prevent stale connection state.

Set to 0 to disable (connections live forever). Lower values (e.g., 600-1800s) for environments with frequent topology changes.

### 3.15 Max Packet Size

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `pkt_buf` | 4096 | Internal packet buffer size (bytes) |
| `max_packet_size` | 2147483647 (2GB) | Maximum PG protocol packet allowed through |
| `sbuf_loopcnt` | 5 | Processing iterations per connection before yielding |

`pkt_buf` affects memory per connection. Lower = more connections in same RAM. `sbuf_loopcnt` controls fairness — higher values favor throughput for active connections, lower values improve latency fairness across connections.

---

## 4. Gap Analysis vs pg-retest Proxy

Areas where the pg-retest proxy may have gaps compared to these production proxies:

### 4.1 Connection Management Gaps
- [ ] **Configurable connection limits** — per-source, per-total, per-backend
- [ ] **Connection backlog / listen queue** tuning
- [ ] **Connection queuing** with timeout (queue clients when backend saturated instead of rejecting)
- [ ] **Connection draining** for graceful shutdown (stop accepting new, drain existing)

### 4.2 Timeout Gaps
- [ ] **Connect timeout** to backend PG
- [ ] **Client idle timeout** (detect dead clients)
- [ ] **Server idle timeout** (detect dead backends)
- [ ] **Query timeout** (kill long-running queries)
- [ ] **Idle-in-transaction timeout** (prevent connection hoarding)
- [ ] **Login/auth timeout** (prevent slowloris on handshake)
- [ ] **Hard stop deadline** (force exit if graceful shutdown stalls)

### 4.3 Health / Resilience Gaps
- [ ] **Backend health checking** (periodic PG protocol-level pings)
- [ ] **Graceful shutdown** with connection drain (SIGTERM handling)
- [ ] **Connection recycling** (server_lifetime equivalent)
- [ ] **Retry logic** for failed backend connections

### 4.4 Security Gaps
- [ ] **Per-source rate limiting** (connections/sec, concurrent connections)
- [ ] **Max packet size enforcement** (prevent oversized queries/results)
- [ ] **Authentication timeout** (bound the handshake phase)
- [ ] **TLS session caching** for performance
- [ ] **OCSP stapling** (not critical for internal DB proxy)

### 4.5 Performance Gaps
- [ ] **TCP keepalive** with configurable intervals (dead connection detection)
- [ ] **Zero-copy / splice** for data forwarding (Linux splice() syscall)
- [ ] **Buffer size tuning** (configurable per-connection buffer)
- [ ] **TCP_DEFER_ACCEPT** (don't accept() until data arrives)
- [ ] **SO_REUSEPORT** for multi-process scaling
- [ ] **Configurable listen backlog**

### 4.6 Observability Gaps
- [ ] **Connection stats** (active, idle, waiting, total)
- [ ] **Per-connection timing** (connect latency, query latency)
- [ ] **Rate metrics** (connections/sec, queries/sec, bytes/sec)
- [ ] **Pool state visibility** (how many backend connections, utilization)
- [ ] **Structured logging** with connection lifecycle events
