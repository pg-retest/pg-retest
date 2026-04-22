# Changelog

All notable changes to pg-retest are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.0.0-rc.4] — 2026-04-22

This release candidate completes the SQL parsing upgrade (shared `SqlLexer`
plus libpg_query-backed structural analysis) and closes two rc.4 ship-blockers
discovered during end-to-end testing against a real Go + pgx application:
the proxy shutdown hang (SC-013) and binary bind-parameter capture failure
(SC-014).

### Added

- **libpg_query (pg_query.rs 6.1.1) dependency** for AST-backed RETURNING
  detection and injection. Requires a C compiler + libclang at build time
  (documented in README Build prerequisites). CI installs libclang-dev
  automatically. Binary size growth documented in commit aa8e92a.
- **`legacy-returning` Cargo feature flag** compiles the pre-Phase-2
  hand-rolled has_returning/inject_returning instead of the new pg_query-
  backed impls. Rollback safety net; scheduled for removal in the release
  after rc.4 per SC-011.
- **pg_query equivalence harness** (`tests/pg_query_equivalence.rs`) over
  a 102-query corpus covering SELECTs, DML variants, CTEs, DDL, MERGE, and
  pg_catalog queries. Asserts has_returning agrees with a direct pg_query
  AST oracle on every corpus entry.
- **inject_returning corpus test** (`tests/sql_returning_corpus.rs`) with
  16 shapes including ON CONFLICT DO NOTHING / DO UPDATE, multi-row
  VALUES, INSERT-SELECT, DEFAULT VALUES, schema-qualified, composite PK,
  trailing comment/semicolon.

### Fixed

- **PII leak in `mask_sql_literals` for tagged dollar-quoted strings.** The
  pre-rc.4 hand-rolled scanner only recognized `$$...$$` and not the tagged
  form `$tag$...$tag$`. For queries containing tagged dollar quotes, the inner
  string was masked (`$S`) but the surrounding `$tag$...$tag$` delimiters were
  left intact, exposing to log consumers that a quoted string existed and its
  position in the query. The rc.4 `SqlLexer` recognizes both forms and masks
  the whole token as `$S`. Applies to both proxy capture and CSV-log capture
  paths. Documented in `tests/sql_lexer_mask_snapshot.rs` module doc.
- **`inject_returning` + ON CONFLICT produced invalid SQL in all prior
  versions.** The legacy hand-rolled impl (and the initial Phase 2 AST
  impl that inherited the wrong expected-output from a legacy unit test)
  placed RETURNING BEFORE ON CONFLICT, producing `INSERT ... VALUES (...)
  RETURNING id ON CONFLICT (id) DO UPDATE ...` which PostgreSQL rejects
  with "syntax error at or near 'ON'". The PG grammar requires RETURNING
  last: `INSERT ... [ ON CONFLICT ... ] [ RETURNING ... ]`. The bug
  shipped latent because `--id-capture-implicit` + `INSERT ... ON
  CONFLICT` is an uncommon combination, and prior unit tests asserted
  string-pattern shape rather than PG parser acceptance. Caught by the
  Phase 2 Docker demo E2E (DC-004) running captured ON CONFLICT traffic
  through real PostgreSQL. Fixed for both AST and legacy impls in commit
  d2b06a3. Live-verified against postgres:16.
- **Proxy graceful shutdown hang (SC-013).** The drain-timeout path
  logged `WARN "Shutdown timeout: N connection(s) still active, forcing
  close"` but `force_close` did not actually cancel the tokio relay
  tasks — the process hung indefinitely whenever a pooled backend
  connection held open a socket read, and the captured `.wkl` file was
  never written. SIGTERM was ignored; only SIGKILL terminated the
  process, losing the entire capture. Fixed by threading
  `CancellationToken` through `run_listener` and `handle_connection` in
  all four `run_proxy*` variants. The token cascades to every spawned
  per-session task so their relay loops bail out via a new shutdown arm
  on the final `tokio::select!` instead of blocking on socket reads.
  Post-fix E2E against the same scenario: SIGINT → "All connections
  drained" → profile written in ~4s vs. infinite hang before.
- **Binary bind-parameter capture for extended-query-protocol clients
  (SC-014).** Any driver that used PG binary format bind parameters
  (pgx, libpq with prepared statements, asyncpg, psycopg3, JDBC) was
  producing unusable captures: `format_bind_params` stringified non-UTF8
  param bytes to the literal placeholder `'<binary N bytes>'`, which
  got substituted into the captured SQL and then rejected by PG on
  replay with `ERROR: invalid input syntax for type X: "<binary N
  bytes>"`. First E2E against a real Go + pgx application produced
  845,146 errors and the replay never completed; 0% functional replay.
  The bug hid behind pgbench and simple-protocol tests for months
  because neither uses the Bind message path. Fixed with a type-aware
  binary decoder at capture time in a new module `src/proxy/pg_binary.rs`.
  The proxy now preserves per-parameter format codes from the Bind
  message and correlates type OIDs per prepared statement from two
  sources: the client's Parse message when it declares non-zero OIDs,
  and the server's ParameterDescription response to a client Describe
  Statement. Decoded values substitute into the existing simple-protocol
  replay path — no profile format bump, no replay engine changes.
  Coverage: 24 builtin types (every integer/float width, uuid, numeric
  with full base-10000 digit reconstruction, all temporal types, inet
  and cidr with RFC 5952 IPv6 compression, macaddr, bit strings, bytea,
  json, jsonb, xml, money, interval), 1-D arrays of the scalar types
  with PG array-quoting rules, and extension types with dynamic OIDs
  (pgvector `vector`, `halfvec` with IEEE-754 binary16 decode,
  `sparsevec`) discovered at proxy startup by probing `pg_type`.
  Unknown OIDs still fall back to the legacy placeholder so behavior
  strictly improves. Post-fix E2E against an 85M-query Yonk benchmark
  workload: 99.977% replay success rate, clean compare PASS. See
  `docs/binary-bind-params.md` for the coverage reference and edge
  cases.

### Changed

- **`mask_sql_literals` and `substitute_ids` rewritten on a shared `SqlLexer`.**
  See `docs/plans/2026-04-17-sql-parsing-phase-1.md` and
  `skill-output/mission-brief/Mission-Brief-sql-parsing-upgrade.md`. New module
  `src/sql/` exposes `SqlLexer` (iterator) and `visit_tokens` (zero-alloc
  callback). Eliminates ~300 lines of duplicated character-scanning logic
  across `src/capture/masking.rs` and `src/correlate/substitute.rs`.
- **Negative-number lexing after keyword context** (behavioral). Under the old
  hand-rolled scanners, `WHERE id = -5` was lexed as separate `-` and `5`
  tokens; masking produced `-$N` (instead of `$N`) and `substitute_ids` looked
  up key `"5"` instead of `"-5"`. The new lexer treats `-5` as one `Number`
  token after operator-like punctuation or start-of-input. Real-world impact
  is nil for captured integer primary keys (positive), but consumers that diff
  masked output against stored snapshots from rc.3 will see `-$N` → `$N` for
  these queries. Documented in both snapshot tests' module doc comments.
- **Two behavioral divergences gated by gold-snapshot tests.** Any future
  change to mask/substitute output requires a deliberate
  `REGEN_SNAPSHOTS=1 cargo test` step, making silent behavior drift
  difficult.
- **`has_returning` now uses libpg_query** by default. Correctly handles
  CTE-wrapped writes (`WITH x AS (INSERT ... RETURNING id) SELECT ...`),
  columns aliased as `"returning"`, the word `RETURNING` inside comments
  or string literals, and MERGE with a returning list — all bug classes in
  the legacy impl.
- **`inject_returning` now uses libpg_query** by default. Splices RETURNING
  at the END of the INSERT statement per PG grammar (after ON CONFLICT if
  present, excluding trailing whitespace/comments/semicolons). Comments
  and whitespace preserved — no deparse. CTE-wrapped inserts return None.

### Notes

- The shared lexer adds ~100–200 ns/query overhead versus the pre-migration
  monolithic state machine (worst case ~150 ns absolute). At 10k qps proxy
  load that is ~0.15 % CPU per thread — below the noise floor of network and
  database latency. Retained as the price of eliminating duplicated scanner
  code and closing the tagged dollar-quote PII leak. See SC-005 (revised) in
  the mission brief.

## [1.0.0-rc.3] — 2026-04-15

This release candidate closes the remaining items from the proxy layer gap
analysis (see `docs/plans/2026-03-27-proxy-layer-gap-analysis.md`) and fixes
four tuner bugs surfaced during end-to-end testing against a real vLLM server
and a 1M-row dataset.

### Added

- **Proxy `/metrics` endpoint** on the control server, returning Prometheus
  text-exposition format. Counters and gauges wired so far:
  `pg_retest_proxy_connections_total`,
  `pg_retest_proxy_connections_active`,
  `pg_retest_proxy_connections_rejected_total{reason=per_ip|msg_size|other}`,
  `pg_retest_proxy_pool_active`, `pg_retest_proxy_pool_idle`,
  `pg_retest_proxy_backend_degraded`,
  `pg_retest_proxy_backend_healthchecks_ok_total`,
  `pg_retest_proxy_backend_healthchecks_fail_total`,
  `pg_retest_proxy_uptime_seconds`. Traffic counters
  (`queries_total`/`bytes_in_total`/`bytes_out_total`/`errors_total`) are
  present in the struct but not yet incremented from the hot-path relay loops
  (planned follow-up). (M3)
- **Backend health check task** that opens a fresh TCP socket to the target
  every `--health-check-interval` seconds (default 30, 0 disables), sends the
  PG SSLRequest handshake, and flips `backend_degraded=1` after
  `--health-check-fail-threshold` consecutive failures (default 3). A single
  successful probe clears the flag. Credential-free by design: uses TCP +
  SSLRequest, not `SELECT 1`, because the proxy has no database user of its
  own. (M1)
- **Client-facing TLS** via `--client-tls-cert` and `--client-tls-key`. Proxy
  now accepts `SSLRequest` from clients and upgrades the connection to TLS
  using `tokio-rustls::TlsAcceptor`. (C3)
- **Graceful shutdown with connection draining**: `--shutdown-timeout`
  (default 30s) replaces the previous hardcoded 500ms sleep. The listener
  stops accepting new connections, waits for in-flight queries to finish, and
  force-closes any still-active connections when the deadline hits. (H6)
- **Resource-protection limits**: `--max-message-size` (default 64MB) rejects
  oversized PG protocol messages with a PG ErrorResponse, and
  `--max-connections-per-ip` (default 0 = unlimited) rejects excess
  connections from a single source IP. (H1, H2)
- **Pool lifecycle hardening**: `--server-lifetime` (default 3600s) forces
  periodic connection recycling, `--server-idle-timeout` (default 600s)
  background-reaps stale idle connections, and
  `--idle-transaction-timeout` (default 0, disabled) warns when a connection
  appears idle-in-transaction beyond the threshold. (M2, M5)
- **Relay timeouts**: `--client-timeout`, `--server-timeout`, and
  `--auth-timeout` (defaults 300/300/30 seconds) close connections stuck in
  their respective phases. (C1, H3)
- **Socket hardening**: `--listen-backlog` (default 1024), `--connect-timeout`
  (default 5s), plus TCP_KEEPALIVE / TCP_NODELAY applied to both client and
  server sockets. (C4, C5, H4, H5)
- **CHANGELOG.md** (this file). (GAP-019 from prod-ready audit)

### Fixed

- **Tuner `--api-url` handling**: URLs with trailing `/v1`, `/v1/`, `/v1beta`,
  `/v1beta/`, or slashes are now normalized before the provider-specific path
  is appended. Previously, pasting `http://host:8000/v1` (the conventional
  OpenAI-compatible endpoint shape from other tools' configs) produced
  `http://host:8000/v1/v1/chat/completions` and a 404. Applied to Claude,
  OpenAI, Gemini, and Ollama advisors.
- **Tuner Ollama response parser**: now accepts three JSON shapes — direct
  array, single recommendation object, or wrapper object with
  `recommendations`/`tools`/`calls`/`items`/`changes` as the array key — so
  small local models (llama3.2:1b, qwen:0.5b) that don't reliably emit the
  exact array shape the prompt asks for no longer cause parse failures. On
  unrecognized shapes, the error message now includes the first 200 chars of
  the actual model response so debugging doesn't require `-v`.
- **Tuner baseline comparison**: each iteration's `replay_p{50,95,99}` is now
  compared against the baseline REPLAY metrics on the same target, not
  against the source-captured metrics from the original workload profile.
  The previous code measured target-vs-source differences (network path,
  hardware, data volume) on every iteration and flagged every iteration as a
  regression whenever the target was "further" than the source, even when
  the applied change materially improved query latency. This was the reason
  a correct `CREATE INDEX` recommendation was rolling back as a spurious
  regression on realistic datasets.
- **Tuner baseline warmup**: the baseline replay now runs twice, discarding
  the first (cold-cache) pass. Without this, iteration measurements were
  systematically biased because the baseline was artificially slow from a
  cold buffer cache while each subsequent iteration hit a warm one.
- **Test certificate hygiene**: the committed self-signed EC test certificate
  and private key in `tests/fixtures/test-cert.pem` and `test-key.pem` have
  been removed from the working tree and replaced with runtime generation
  via `rcgen` inside the TLS acceptor test. `.gitignore` now excludes
  `*.pem`, `*.key`, `*.p12`, `.env*`, `data/`, and `proxy-capture.db`.
  *The deleted files still exist in git history and will be purged via
  `git filter-repo` before the public 1.0.0 launch announcement.*

### Changed

- `--duration` signal handler on the proxy now logs rather than panicking on
  signal handler install failure (cosmetic, was only reachable in unusual
  container environments).
- Proxy gap analysis document (`docs/plans/2026-03-27-proxy-layer-gap-analysis.md`)
  and research notes (`docs/plans/proxy-layer-best-practices-research.md`)
  added to the repository so the rationale for the hardening sprint is
  discoverable without git archaeology.

### Internal

- 16 new unit tests for `proxy::metrics` and `proxy::health`.
- 9 new unit tests for `tuner::advisor::normalize_base_url` and
  `tuner::advisor::parse_ollama_recommendations`.
- Two pre-existing `cargo clippy -D warnings` warnings in
  `tests/replay_test.rs` and `tests/transform_test.rs` auto-fixed.
- `docs/plans/2026-03-27-proxy-layer-gap-analysis.md` updated to track the
  resolved items (C1-C5, H1-H6, M2, M5, M3, M1, C3).
- Full test suite: **398 passed, 0 failed** (was 373 at the start of rc.2).

### Verification

End-to-end tested on PostgreSQL 16 against a real 1M-row dataset:

- `capture` → `replay` → `compare`: 181 queries, 0 errors, PASS
- `tune --api-url http://vllm:8000/v1`: reached provider, parsed response,
  ran the full warmup+measurement baseline sequence, applied and kept a
  valid index recommendation (p50 −92.2%, p95 −97.8%, p99 −97.5%)
- Proxy `/metrics`: 4 real psql connections through the proxy correctly
  incremented `connections_total=4` and returned `connections_active=0` on
  disconnect
- Backend health check: killing the target container flipped
  `backend_degraded` from 0 → 1 within ~4 seconds; restarting flipped it
  back to 0 on the next probe

## [1.0.0-rc.2] — 2026-04-12

First release candidate with the full proxy hardening surface area. This
tag was never published externally; the version bump precedes most of the
hardening work that landed in rc.3.

### Added

- Initial version bump from 0.x to 1.0.0-rc.2 in preparation for the GA
  launch candidate track.

## [1.0.0-rc.1] — Pre-tag history

Prior to rc.2, the project was tracked as 0.x and did not have formal
release tags. The cumulative feature work in that period included:

- **M1 — Capture & Replay.** PostgreSQL CSV log parser, wire-protocol proxy
  with session-mode pooling, transaction-aware replay engine with
  auto-rollback, PII masking state machine, exit-code-based comparison
  reports.
- **M2 — Scaled Benchmark.** Workload classification (Analytical /
  Transactional / Mixed / Bulk), uniform and per-category session scaling,
  capacity planning reports with throughput and p50/p95/p99 latency.
- **M3 — CI/CD Integration.** TOML pipeline config, Docker provisioner,
  threshold evaluator, JUnit XML output, A/B variant mode with winner
  determination, staged exit codes (0–5).
- **M4 — Cross-Database Capture.** MySQL slow query log parser, composable
  SQL transform pipeline with regex-based rules for backticks → double
  quotes, `LIMIT x, y` → `LIMIT y OFFSET x`, `IFNULL` → `COALESCE`,
  `IF()` → `CASE WHEN`, `UNIX_TIMESTAMP` → `EXTRACT(EPOCH FROM ...)::bigint`.
  AWS RDS/Aurora capture via `aws` CLI with paginated log download.
- **M5 — AI-Assisted Tuning.** Multi-provider LLM tuning (Claude, OpenAI,
  Gemini, Bedrock, Ollama) with a control-loop architecture: collect
  pg_settings/schema/pg_stat_statements/EXPLAIN context, request
  recommendations, validate against a ~46-parameter safety allowlist,
  apply, replay, measure, auto-rollback on p95 regression, iterate.
  Production hostname check, dry-run default, tuning report persistence.
- **Web Dashboard.** Axum + SQLite + WebSocket. Twelve pages (dashboard,
  workloads, proxy, replay, A/B, compare, pipeline, history, transform,
  tuning, help, demo) with real-time updates. Bearer token auth,
  auto-generated on startup. Default bind 127.0.0.1.
- **Docker Demo.** Compose file with pg-retest + two seeded PG instances.
  5-step wizard and 4 scenario cards. `PG_RETEST_DEMO=true` env gate.
- **Workload Transform.** AI-powered workload transformation
  (`transform analyze|plan|apply`). Three-layer architecture: deterministic
  analyzer (Union-Find table grouping), multi-provider LLM planner,
  deterministic engine (weighted session duplication, query injection,
  group removal). TOML transform plans as an intermediate artifact.
- **ID Correlation.** Tiered `--id-mode none|sequence|correlate|full`,
  proxy RETURNING capture, cross-session `DashMap` ID map, SQL substitution
  state machine, stealth mode, auto-inject RETURNING, sequence
  snapshot/restore, automatic restore-point creation.

For a complete commit-level log of pre-rc.2 development, see
`git log 1a582b4~1` on the `main` branch.

[Unreleased]: https://github.com/pg-retest/pg-retest/compare/v1.0.0-rc.4...HEAD
[1.0.0-rc.4]: https://github.com/pg-retest/pg-retest/releases/tag/v1.0.0-rc.4
[1.0.0-rc.3]: https://github.com/pg-retest/pg-retest/releases/tag/v1.0.0-rc.3
[1.0.0-rc.2]: https://github.com/pg-retest/pg-retest/releases/tag/v1.0.0-rc.2
[1.0.0-rc.1]: https://github.com/pg-retest/pg-retest/releases/tag/v1.0.0-rc.1
