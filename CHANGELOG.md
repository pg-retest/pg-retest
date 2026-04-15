# Changelog

All notable changes to pg-retest are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

No unreleased changes.

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

[Unreleased]: https://github.com/pg-retest/pg-retest/compare/v1.0.0-rc.3...HEAD
[1.0.0-rc.3]: https://github.com/pg-retest/pg-retest/releases/tag/v1.0.0-rc.3
[1.0.0-rc.2]: https://github.com/pg-retest/pg-retest/releases/tag/v1.0.0-rc.2
[1.0.0-rc.1]: https://github.com/pg-retest/pg-retest/releases/tag/v1.0.0-rc.1
