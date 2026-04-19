# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**pg-retest** (working title for EDB Database Testing Kit / EDTK) is a tool for capturing, replaying, and scaling PostgreSQL database workloads. It enables users to validate performance across configuration changes, server migrations, and scaling scenarios.

### Core Capabilities (by milestone)

1. **Capture & Replay** — Capture SQL workload from a PG server (per-connection thread profiling), replay it against a backup database, produce side-by-side performance comparison. Support read-write and read-only (strip DML) modes.
2. **Scaled Benchmark** — Classify captured workload into categories (analytical, transactional, etc.), scale each independently to simulate increased traffic for capacity planning.
3. **CI/CD Integration** — Automate the capture/replay/compare cycle as a pipeline step with pass/fail thresholds.
4. **Cross-Database Capture** — Capture from Oracle, MySQL, MariaDB, SQL Server and transform into PG-compatible workload for replay.
5. **AI-Assisted Tuning** — Use AI to recommend config, schema, and query changes; test iterations and produce comparison reports.

### Key Design Constraints

- Workload capture must have minimal impact on production systems.
- Transactions change data, which changes query plans. For accurate 1:1 replay, restore from a point-in-time backup before replay.
- Two distinct modes are needed: **true replay** (exact 1:1 reproduction) and **simulated benchmark** (scaled workload generation).
- PII may appear in captured queries — the tool must support filtering/masking.
- Thread simulation fidelity degrades at high scale; benchmark mode accepts this tradeoff.

## Architecture

```
┌─────────────┐    ┌──────────────┐    ┌──────────────┐    ┌────────────┐
│   Capture    │───>│   Workload   │───>│    Replay     │───>│  Reporter  │
│   Agent      │    │   Profile    │    │    Engine     │    │            │
└─────────────┘    │   (storage)  │    └──────────────┘    └────────────┘
                   └──────────────┘
```

- **Capture Agent** — Connects to PG (via `pg_stat_activity` polling, log parsing, or proxy) to record per-connection SQL streams with timing metadata.
- **Workload Profile** — Serialized representation of captured workload: queries, connection/thread mapping, timing, dependencies, transaction boundaries.
- **Replay Engine** — Reads a workload profile and replays it against a target PG instance, preserving connection parallelism and timing. Supports replay modes (exact, read-only, scaled).
- **Reporter** — Compares source vs. replay metrics and produces a performance comparison report (per-query latency, throughput, errors, regressions).

## Build & Development

- **Language:** Rust (2021 edition)
- **Build:** `cargo build` (debug) / `cargo build --release`
- **Test all:** `cargo test`
- **Test single file:** `cargo test --test profile_io_test`
- **Test single function:** `cargo test --test profile_io_test test_profile_roundtrip_messagepack`
- **Test lib unit tests:** `cargo test --lib capture::csv_log`
- **Lint:** `cargo clippy`
- **Format:** `cargo fmt`
- **Run:** `cargo run -- <subcommand> [args]`
- **Verbose logging:** `RUST_LOG=debug cargo run -- -v <subcommand>`

### Crate Structure

The project is both a library (`src/lib.rs`) and binary (`src/main.rs`). Integration tests in `tests/` import from the library crate via `use pg_retest::...`. The binary crate handles CLI dispatch only.

Key modules:
- `capture::csv_log` — PG CSV log parser (pluggable backend via `CaptureSource` pattern)
- `capture::masking` — SQL literal masking for PII protection (strings→`$S`, numbers→`$N`)
- `profile` — Core data types (`WorkloadProfile`, `Session`, `Query`) + MessagePack I/O (v2 format with transaction support)
- `replay::session` — Async per-session replay engine (Tokio + tokio-postgres), transaction-aware (auto-rollback on failure)
- `replay::scaling` — Session duplication with staggered offsets for load testing; per-category scaling via `scale_sessions_by_class()`
- `classify` — Workload classification (Analytical/Transactional/Mixed/Bulk) based on read/write ratio, latency, transaction count
- `compare` — Performance comparison logic + terminal/JSON reporting + exit code evaluation
- `compare::capacity` — Scaled replay reporting (throughput QPS, latency percentiles, error rate)
- `config` — TOML pipeline config parsing and validation (`PipelineConfig`, `ThresholdConfig`, etc.)
- `pipeline` — CI/CD pipeline orchestrator (capture → provision → replay → compare → threshold → report)
- `provision` — Docker provisioner via CLI subprocess (start/teardown containers, backup restore)
- `compare::threshold` — Threshold-based pass/fail evaluation (p95, p99, error rate, regression count)
- `compare::junit` — JUnit XML output for CI test result integration
- `transform` — Composable SQL transform pipeline (`SqlTransformer` trait, `TransformPipeline`) for cross-database SQL conversion
- `transform::mysql_to_pg` — MySQL-to-PostgreSQL transform rules (backticks→double quotes, LIMIT rewrite, IFNULL→COALESCE, IF→CASE WHEN, UNIX_TIMESTAMP→EXTRACT)
- `capture::mysql_slow` — MySQL slow query log parser (`MysqlSlowLogCapture`) with integrated transform pipeline
- `capture::rds` — AWS RDS/Aurora log capture via `aws` CLI (paginated download + CsvLogCapture delegation)
- `compare::ab` — A/B variant comparison (per-query regression detection, winner determination, terminal/JSON reporting)
- `transform::plan` — TransformPlan data types (TOML/JSON serde): QueryGroup, TransformRule (Scale/Inject/InjectSession/Remove), PlanSource, PlanAnalysis
- `transform::analyze` — Deterministic workload analyzer: `extract_tables()` (regex-based), `extract_filter_columns()`, `analyze_workload()` (Union-Find table grouping), produces `WorkloadAnalysis` for LLM context
- `transform::engine` — Deterministic transform engine: `apply_transform()` applies a TransformPlan to a WorkloadProfile (weighted session duplication, query injection with seeded RNG, group removal)
- `transform::planner` — Multi-provider LLM planner: `LlmPlanner` async trait with `ClaudePlanner` (tool_use), `OpenAiPlanner` (function_calling), `GeminiPlanner` (functionDeclarations), `BedrockPlanner` (AWS CLI Converse), `OllamaPlanner` (JSON mode). Direct HTTP via reqwest (Bedrock via AWS CLI subprocess).
- `tuner` — AI-assisted tuning orchestrator (configurable loop: context → LLM → safety → apply → replay → compare → auto-rollback on regression)
- `tuner::types` — Recommendation, TuningConfig, TuningIteration, TuningReport, TuningEvent (including RollbackStarted/RollbackCompleted)
- `tuner::context` — PG introspection (pg_settings, schema, pg_stat_statements, EXPLAIN plans)
- `tuner::advisor` — TuningAdvisor trait with Claude/OpenAI/Gemini/Bedrock/Ollama providers (120s request timeout)
- `tuner::safety` — Parameter allowlist, blocked operations, production hostname check
- `tuner::apply` — Recommendation application with rollback tracking (ConfigChange via ALTER SYSTEM RESET, CreateIndex via DROP INDEX)
- `correlate` — ID correlation module for database-generated ID handling during replay
- `correlate::sequence` — `SequenceState`, `snapshot_sequences()`, `restore_sequences()` — PG sequence snapshot at capture time, setval() restore before replay
- `correlate::capture` — `ResponseRow`, `TablePk`, `has_returning()`, `inject_returning()`, `is_currval_or_lastval()`, `discover_primary_keys()` — RETURNING detection, auto-injection, PK discovery
- `correlate::map` — `IdMap` wrapper around `Arc<DashMap>` for lock-free cross-session ID mapping. `register()`, `substitute()`, `get()`, `len()`
- `correlate::substitute` — SQL substitution state machine (character-level lexer with positional context filtering). `substitute_ids()` rewrites SQL literals using the ID map.
- `cli` — Clap derive-based CLI argument structs (11 subcommands: capture, replay, compare, inspect, proxy, run, ab, web, transform, tune)
- `web` — Axum HTTP server + WebSocket dashboard (embedded static files via rust-embed, SQLite metadata via rusqlite)
- `web::db` — SQLite schema + CRUD for workloads, runs, proxy_sessions, threshold_results, tuning_reports
- `web::state` — `AppState` (db, data_dir, ws_broadcast, task_manager)
- `web::tasks` — `TaskManager` for background ops (proxy, replay, pipeline) with CancellationToken
- `web::ws` — WebSocket handler with `WsMessage` enum (ProxyStarted, ReplayProgress, PipelineCompleted, TuningIterationStarted, TuningRecommendations, RollbackStarted, RollbackCompleted, etc.)
- `web::routes` — Router construction with all `/api/v1/` route nesting
- `web::handlers::workloads` — Upload, import, list, inspect, classify, delete workload profiles
- `web::handlers::replay` — Start/cancel/status replay with progress broadcast
- `web::handlers::compare` — Compute comparison report, store/retrieve
- `web::handlers::proxy` — Start/stop proxy with WS live traffic
- `web::handlers::ab` — A/B test start/status (sequential replay per variant)
- `web::handlers::pipeline` — Pipeline config validation + execution
- `web::handlers::runs` — Historical run listing, filtering, trends
- `web::handlers::transform` — Transform API endpoints (analyze, plan, apply) for web dashboard
- `web::handlers::tuning` — Tuning API endpoints (start, status, cancel) for web dashboard
- `web::handlers::demo` — Demo mode handlers (wizard steps, scenario cards, DB reset)
- `proxy::staging` — SQLite staging for capture data (batched insert, read-back, cleanup, crash recovery)
- `proxy::control` — Minimal HTTP control endpoint for standalone persistent proxy, exposes `/status` and `/metrics`
- `proxy::metrics` — Prometheus-shaped `ProxyMetrics` struct with atomic counters for connections (total/active/rejected), queries, bytes in/out, errors, backend health, uptime, plus a `render(pool_active, pool_idle)` function returning text-exposition format
- `proxy::health` — Credential-free backend health check task. Periodic TCP + PG SSLRequest probe; flips `backend_degraded` after N consecutive failures. Configured via `--health-check-interval`, `--health-check-timeout`, `--health-check-fail-threshold`
- `sql::lex` — Shared SQL lexer (`SqlLexer` iterator + `visit_tokens` zero-alloc callback + `Token`/`TokenKind`/`Span` types). Byte-offset-based, iterator-pattern with ASCII fast paths for Whitespace/Ident. Consumed by `capture::masking` and `correlate::substitute`. Does NOT attempt structural parsing — token boundaries only (Whitespace, Ident, StringLiteral, DollarString, QuotedIdent, Number, BindParam, LineComment, BlockComment, Punct). See `docs/plans/2026-04-17-sql-parsing-phase-1.md` and `skill-output/mission-brief/Mission-Brief-sql-parsing-upgrade.md`.
- `sql::ast` — libpg_query-backed AST helpers for structural SQL analysis (`has_returning(sql) -> Result<bool>`, `inject_returning(sql, pk_map) -> Result<Option<String>>`). Hot-path callers use the cheap `might_have_returning` prefix pre-filter before dispatching to pg_query (~4-8µs per parse). The public `correlate::capture::has_returning` and `inject_returning` wrappers preserve the bool/Option<String> shape by mapping Err to the safe default (false / None). Legacy hand-rolled impls retained behind `--features legacy-returning` for one release cycle per SC-011.
- `tls` — TLS connector builder (`TlsMode` enum: Disable/Prefer/Require), used by replay and tuner. `make_tls_connector()` with optional CA cert path.
- `web::auth` — Bearer token auth middleware for Axum dashboard

## Milestone Status

- **M1: Capture & Replay** — Complete (with gap closure). CSV log capture, proxy capture, transaction boundaries, PII masking, async replay with transaction-aware error handling, comparison reports with exit codes.
- **M2: Scaled Benchmark** — Complete. Workload classification (Analytical/Transactional/Mixed/Bulk), session scaling with stagger, per-category scaling (`--scale-analytical 2 --scale-transactional 4`), capacity planning reports.
- **M3: CI/CD Integration** — Complete. TOML config (`pg-retest run --config .pg-retest.toml`), Docker provisioner via CLI subprocess, JUnit XML output, threshold evaluation, pipeline orchestrator with staged exit codes (0-5), A/B variant mode via `[[variants]]` in pipeline config.
- **M4: Cross-Database Capture (MySQL)** — Complete. MySQL slow query log parser, composable SQL transform pipeline (regex-based: backticks, LIMIT, IFNULL, IF, UNIX_TIMESTAMP), CLI `--source-type mysql-slow`, pipeline config support.
- **Gap Closure** — Complete. Per-category scaling, A/B variant testing (`pg-retest ab`), cloud-native capture from AWS RDS/Aurora (`--source-type rds`).
- **Web Dashboard** — Complete. Axum + Alpine.js + Chart.js SPA (`pg-retest web --port 8080`). 11 pages: dashboard, workloads, proxy, replay, A/B, compare, pipeline, history, transform, tuning, help. WebSocket real-time updates. SQLite metadata storage.
- **Docker Demo** — Complete. Docker Compose with pg-retest + db-a (seeded) + db-b (seeded). E-commerce schema (5 tables, ~94k rows). Demo page with 5-step wizard + 4 scenario cards. `PG_RETEST_DEMO=true` env var activation.
- **Workload Transform** — Complete. AI-powered workload transformation (`pg-retest transform analyze|plan|apply`). 3-layer architecture: deterministic Analyzer (Union-Find table grouping), multi-provider LLM Planner (Claude/OpenAI/Gemini/Bedrock/Ollama), deterministic Engine (weighted session duplication, query injection, group removal). TOML transform plans as intermediate artifact. Design at `docs/plans/2026-03-07-workload-transform-design.md`. 201 tests.
- **M5: AI-Assisted Tuning** — Complete. Multi-provider LLM tuning (`pg-retest tune`). Configurable loop: collect PG context → LLM recommendations → safety validation → apply → replay → compare → auto-rollback on p95 regression → iterate. 4 recommendation types (config, index, query rewrite, schema). Safety allowlist (~41 safe PG params), production hostname check. 5 providers: Claude/OpenAI/Gemini/Bedrock/Ollama. Tuning report persistence to SQLite. Dry-run default. Web dashboard tuning page with history and recommendations. 232 tests.
- **ID Correlation** — Complete. Tiered `--id-mode` (none/sequence/correlate/full), proxy RETURNING capture, cross-session DashMap, SQL substitution state machine, stealth mode, auto-inject RETURNING, sequence snapshot/restore, auto restore point creation. 176 tests, 6 criterion benchmarks.

## Gotchas

- All `pub mod` declarations go in `src/lib.rs`, not `src/main.rs` — integration tests import from the library crate.
- PG CSV log timestamps (`2024-03-08 10:00:00.100 UTC`) are not RFC 3339 — the parser has a fallback via `NaiveDateTime`.
- Capture backends are pluggable: implement parsing in `src/capture/`, the profile format and replay engine don't change.
- Always run `cargo fmt` after writing code — the formatter's output may differ from hand-written style.
- `.wkl` files are MessagePack binary (v2 format). Use `pg-retest inspect file.wkl` to view as JSON.
- Profile format v2 adds `transaction_id: Option<u64>` to `Query`. v1 files deserialize cleanly via `#[serde(default)]`.
- `QueryKind` now includes `Begin`, `Commit`, `Rollback` variants — existing tests that asserted `BEGIN` → `Other` were updated to expect `Begin`.
- PII masking (`--mask-values`) uses a hand-written character-level state machine, not regex. This handles SQL edge cases (escaped quotes, dollar-quoting, identifiers with numbers) correctly.
- Scaling write workloads (`--scale N` with DML) prints a safety warning — scaled writes execute multiple times and change data state.
- MySQL capture: `--source-type mysql-slow` enables the transform pipeline automatically. `SHOW`, `SET NAMES`, `USE` and other MySQL-specific commands are skipped (not included in the profile).
- SQL transforms use regex (not `sqlparser`). This covers ~80-90% of real MySQL queries. Known limitations: backtick replacement inside string literals, single LIMIT rewrite per query.
- The `capture_method` field in WorkloadProfile distinguishes sources: `"csv_log"` for PG, `"mysql_slow_log"` for MySQL, `"rds"` for AWS RDS/Aurora.
- Per-category scaling (`--scale-analytical`, etc.) is mutually exclusive with uniform `--scale N`. Per-category takes priority if any class flag is set; unspecified classes default to 1x.
- A/B variant testing: CLI uses `--variant "label=connstring"` (2+ required). Pipeline uses `[[variants]]` in TOML config. When variants are present, the pipeline bypasses normal provisioning and runs sequential replay against each target.
- RDS capture requires the `aws` CLI to be installed and configured. Pagination uses `--marker` (not `--starting-token`). Log files >1MB are downloaded in chunks via `download-db-log-file-portion`.
- `WorkloadClass` derives `Hash` (needed for use as `HashMap` key in per-category scaling).
- Web dashboard: static files are embedded via `rust-embed` from `src/web/static/`. Changes to JS/CSS/HTML require recompilation.
- Web dashboard: SQLite stores metadata only — `.wkl` files remain source of truth on disk in `data_dir/workloads/`.
- Web dashboard: Background tasks (proxy, replay, pipeline) use `TaskManager` with `CancellationToken`. WebSocket broadcast channel pushes events to all connected clients.
- Web dashboard: Frontend uses Alpine.js (reactivity), Chart.js (graphs), Tailwind CSS (styling) all via CDN — no build step required.
- Web dashboard: Uses `OnceLock<Arc<RwLock<ProxyState>>>` for proxy state tracking (module-level static).
- Tuner: default is dry-run (`--apply` required to execute). Safety allowlist blocks ~50 dangerous PG params.
- Tuner: baseline is collected via replay before any tuning iteration (comparison is always vs. baseline, not vs. source timing).
- Tuner: `pg_stat_statements` is optional — if the extension isn't installed, `stat_statements` will be `None`.
- Tuner: EXPLAIN is only run for SELECT queries without bind parameters (queries with `$1` are skipped).
- Tuner: production hostname check blocks targets containing "prod", "production", "primary", "master", "main" without `--force`.
- Tuner: SchemaChange SQL is split on semicolons and executed per-statement for better error reporting.
- Tuner: OpenAI newer models (gpt-5, o-series) use `max_completion_tokens`; older models use `max_tokens`. Model prefix detection handles this automatically.
- Tuner: Bedrock provider uses `aws bedrock-runtime converse` CLI subprocess (no AWS SDK crate dependency). Uses standard AWS credentials (env vars, profiles, IAM roles).
- Tuner: Gemini provider uses `x-goog-api-key` header auth and `functionDeclarations` format. Set `GEMINI_API_KEY` env var or use `--api-key`.
- Tuner: tuning reports are persisted to SQLite `tuning_reports` table and also create a `runs` entry (type="tuning") for history tracking.
- Tuner: auto-rollback triggers when p95 latency regresses >5% after applying changes. Rollback reverses ConfigChange via `ALTER SYSTEM RESET` + `pg_reload_conf()` and CreateIndex via `DROP INDEX`.
- Tuner: LLM request timeout is 120s for all advisor providers. Transform planner timeouts are shorter (Claude/OpenAI 30s, Gemini/Bedrock 60s).
- Tuner: default model versions — Claude: `claude-sonnet-4-20250514`, OpenAI: `gpt-4o`, Gemini: `gemini-2.5-flash`, Bedrock: `us.anthropic.claude-sonnet-4-20250514-v1:0`, Ollama: `llama3`.
- Web dashboard: tuning page shows previous sessions with expandable recommendation details, applied/failed/dry-run badges, and comparison stats (p50/p95/p99 changes).
- Web dashboard: API handlers return structured JSON error bodies (not bare status codes). Frontend handles empty/non-JSON error responses gracefully.
- Transform: `extract_tables()` groups tables by co-occurrence within a single SQL statement (Union-Find), not by session co-occurrence. Two tables in separate queries won't be grouped unless a third query touches both.
- Transform: The engine's Remove operation uses `query_indices` from the plan's group definition to identify which queries to remove. Empty `query_indices` means nothing is removed.
- Transform: Transform plans are TOML files with `#[serde(tag = "type")]` on `TransformRule` enum. The `type` field must be `scale`, `inject`, `inject_session`, or `remove`.
- Transform: `apply_transform()` is deterministic when given the same seed. Default seed is derived from the plan's prompt string hash.
- Transform: LLM planner uses `reqwest` for direct HTTP calls (Bedrock via AWS CLI subprocess). API key resolution: `--api-key` flag → `ANTHROPIC_API_KEY` env → `OPENAI_API_KEY` env → `GEMINI_API_KEY` env. Bedrock uses standard AWS credentials (env/profile/IAM role).
- Transform: `--dry-run` on `transform plan` shows the analyzer output and system prompt without calling the LLM API.
- Demo mode: requires `PG_RETEST_DEMO=true` env var. Connection strings via `DEMO_DB_A`, `DEMO_DB_B`. Workload path via `DEMO_WORKLOAD`.
- Demo page: wizard step state and scenario results are stored in-memory (reset on server restart).
- Demo DB reset: drops and recreates all tables in DB-B by re-running init-db-b.sql.
- Proxy persistent mode: `--persistent` keeps proxy running, capture toggled via `proxy-ctl` or web UI. Without `--persistent`, behavior is identical to before.
- Proxy staging: capture data staged to SQLite (`capture_staging` table) instead of in-memory. Batched inserts (100 queries or 500ms). Crash recovery via `proxy-ctl recover`.
- Proxy control port: standalone persistent proxy exposes HTTP control on `--control-port` (default 9091). Web mode uses existing `/api/v1/proxy/*` endpoints instead.
- `proxy-ctl` auto-detects web vs standalone by trying `GET /api/v1/health`.
- `no_capture` is `Arc<AtomicBool>` shared across all relay connections — toggled at runtime for persistent capture start/stop.
- Proxy `/metrics` endpoint on the control port returns Prometheus text-exposition format. `connections_total`, `connections_active`, `connections_rejected_total{reason}`, `pool_active`, `pool_idle`, `backend_degraded`, `backend_healthchecks_{ok,fail}_total`, `uptime_seconds` are wired. `queries_total`/`bytes_*_total`/`errors_total` are present but not yet incremented from the hot-path relay loops (follow-up).
- Proxy backend health check spawns a background task on startup when `--health-check-interval > 0` (default 30s). Probes the target via `TcpStream::connect` + PG SSLRequest (credential-free). After `--health-check-fail-threshold` consecutive failures (default 3), flips `backend_degraded=1` and logs a WARN. A single success clears the flag.
- Tuner `--api-url` is normalized: trailing `/v1`, `/v1/`, `/v1beta`, `/v1beta/`, and `/` are stripped before appending the provider-specific path, so pasting `http://host:8000/v1` from another tool's config works for all HTTP advisors (Claude, OpenAI, Gemini, Ollama).
- Tuner Ollama response parser accepts three JSON shapes: a top-level array, a single recommendation object, or a wrapper object with `recommendations`/`tools`/`calls`/`items`/`changes` as the key. Small local models (llama3.2:1b, qwen:0.5b) rarely produce the array shape. Garbage responses return an error with the first 200 chars of the model output.
- Tuner baseline: runs replay twice on startup (warmup + measurement pass) so the PostgreSQL buffer cache is warm before the measured baseline. Iteration results are compared against this baseline replay, not against source-side captured timings, so `target != source_host` deployments don't get spurious "everything is a regression" signals.
- Web dashboard: default bind changed from `0.0.0.0` to `127.0.0.1`. Docker containers must use `--bind 0.0.0.0`. Auth token is auto-generated on startup; use `--no-auth` for development.
- TLS: `--tls-mode` defaults to `prefer`. Supported modes: `disable`, `prefer`, `require`. Uses `tokio-postgres-rustls` with system CAs by default, or `--tls-ca-cert` for custom CA.
- Replay: `--max-connections` limits concurrent DB connections via `tokio::sync::Semaphore`. Default unlimited (backward compatible).
- Tuner safety: SchemaChange recommendations use an allowlist (only `CREATE INDEX`, `ANALYZE`, `REINDEX` auto-applied). All other DDL rejected with explanation. SQL values in `ALTER SYSTEM SET` are escaped (single quotes doubled).
- CLI: `--target-env` reads connection string from an environment variable. `--output-format text|json` on compare/inspect/ab commands. `--log-format text|json` global flag for structured JSON logs.
- Web dashboard: passwords in connection strings are redacted (`***`) before storing in SQLite.
- Web dashboard: graceful shutdown via SIGTERM/SIGINT handling. Background tasks cancelled on shutdown.
- Persistent proxy: connections established before capture is toggled ON are now captured (late-join session support).
- API key validation: `tune` command validates API key presence at startup for claude/openai/gemini providers.
- Compare: `compute_comparison()` accepts optional `ReplayMode` to filter source queries. ReadOnly mode no longer includes DML queries in source percentiles.
- `--id-mode` defaults to `none`. Four modes: none, sequence, correlate, full. `full` = sequence + correlate combined.
- `--id-capture-implicit` auto-injects RETURNING for bare INSERTs and intercepts currval/lastval. Off by default. Requires `--source-db` for PK discovery.
- Stealth mode (default when `--id-capture-implicit` is on): proxy strips auto-injected RETURNING DataRows before forwarding to client. Use `--no-stealth` to forward them.
- The proxy auto-creates a `pg_create_restore_point('pg_retest_capture_YYYYMMDD_HHMMSS')` when `--source-db` is provided. Requires superuser or pg_create_restore_point privilege.
- `QueryReturning` events must be sent AFTER `QueryComplete` in the capture pipeline — ordering matters for response_values attachment.
- ID map (`DashMap`) is shared across all replay sessions via `Arc`. Cross-session timing race at tight windows (~4-7% error rate for concurrent write workloads).
- `substitute_ids()` uses single-literal eligibility scope — LIMIT/OFFSET ineligibility resets after consuming one literal.
- Profile format additions are backward-compatible: `response_values: Option<Vec<ResponseRow>>` on Query, `sequence_snapshot: Option<Vec<SequenceState>>` and `pk_map: Option<Vec<TablePk>>` on Metadata.
- `dashmap` crate used for lock-free concurrent ID map.
- PK discovery uses `--source-db` connection string (not `--target` which is host:port format).
- SQL lexer (`src/sql/lex.rs`) is shared by masking and ID substitution. When adding a new call site that needs SQL tokenization, consume `visit_tokens(sql, |kind, text| ...)` for hot paths (zero-alloc) or `SqlLexer::new(sql)` as an iterator when you need `Token { kind, text, span }`. The lexer handles: single-quoted strings with `''` escape, dollar-quoted strings (`$$` and `$tag$`), double-quoted identifiers with `""` escape, line/block comments (EOF-tolerant), numeric literals (int/decimal/scientific/negative-in-numeric-context), bind params (`$N`), Unicode identifiers.
- Phase 1 parity is enforced by gold-snapshot tests in `tests/sql_lexer_mask_snapshot.rs` and `tests/sql_lexer_substitute_snapshot.rs`. Regenerate with `REGEN_SNAPSHOTS=1 cargo test --test <name>` only when intentionally changing mask/substitute behavior.
- The SqlLexer-based `mask_sql_literals` and `substitute_ids` fix a pre-existing PII leak where tagged dollar quotes (`$tag$...$tag$`) were not recognized by the hand-rolled scanner, leaving `$tag$` delimiters visible around masked contents. Also fix negative-number lexing after keyword context (e.g., `WHERE id = -42` is now one Number token). Both changes are documented in the snapshot tests' module doc comments.
- Shared-lexer abstraction tax: `mask_sql_literals` runs ~200ns/query slower than the pre-migration monolithic state machine on worst-case inputs. At 10k qps proxy load that's ~0.2% CPU — accepted as the price of eliminating ~300 lines of duplicated scanner code and closing the PII leak. If a future consumer needs the absolute fastest path, inline the lexer via a macro or hand-roll the specific scanner — don't regress `visit_tokens` for one caller.
- pg_query crate (6.1.1) adds a C dep on libpg_query. Build requires clang/libclang-dev on Debian/Ubuntu; macOS gets it via xcode-select --install. CI installs libclang-dev automatically (see .github/workflows/ci.yml and release.yml). ~1.5MB binary growth once the linker stops dead-stripping (real measurement after Task 5/7 wired pg_query into call paths; negligible for Phase 2 rc.4).
- `legacy-returning` feature flag compiles the pre-Phase-2 hand-rolled has_returning/inject_returning instead of the pg_query-backed ones. Rollback safety net; removed in the release after rc.4 per SC-011.
- has_returning becoming fallible (`Result<bool>`) is implementation-internal. Callers in src/proxy/connection.rs see the old bool signature via the wrapper — Err is mapped to false (matches prior "unknown = assume no RETURNING" behavior).
- inject_returning splice approach: pg_query gives us the AST; we splice RETURNING into the original SQL string (not deparse, which would lose comments/whitespace). RETURNING always goes at the END of the INSERT statement per PG grammar: `INSERT ... [ ON CONFLICT ... ] [ RETURNING ... ]` — an earlier draft placed it before ON CONFLICT, which PostgreSQL rejects with a syntax error. Fixed in d2b06a3 after the Docker demo E2E caught it.
- CTE-wrapped inserts (`WITH x AS (INSERT ...) SELECT ...`) return None from inject_returning — splice semantics ambiguous for Phase 2. Phase 3 may revisit.
- Phase 2 pg_query-backed sites are µs-scale (4-8µs per parse) vs the Phase 1 lexer's ns-scale. Mission brief SC-005 revised (2026-04-18) carves out parse-required cases to a ≤50µs envelope. Only fires on INSERT/UPDATE/DELETE/MERGE/WITH and only when --id-capture-implicit is on. At 10k qps write-heavy proxy: ~4-8% CPU.
- Equivalence harness (tests/pg_query_equivalence.rs) cross-checks has_returning against a direct pg_query AST oracle across a 102-query corpus. Any divergence fails CI. Phase 3 will extend to extract_tables and extract_filter_columns with their own oracles against the same corpus.

## Conventions

- Target PostgreSQL as the primary replay destination for all milestones.
- Workload profiles should be a portable, version-stamped format (not tied to a specific PG version).
- Capture and replay must be decoupled — capture produces a profile file; replay consumes it. They should never require simultaneous access to source and target.
- Connection-level parallelism in replay is critical for realistic results; avoid serializing inherently parallel workloads.
- Configuration changes and server differences are the variables under test — the tool itself should introduce minimal overhead or variance.
- Diagnostic/status messages use `tracing` crate (`info!`, `warn!`, `error!`). User-facing CLI output (reports, tables, JSON) uses `println!`.
