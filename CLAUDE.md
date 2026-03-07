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
- `transform::planner` — Multi-provider LLM planner: `LlmPlanner` async trait with `ClaudePlanner` (tool_use), `OpenAiPlanner` (function_calling), `OllamaPlanner` (JSON mode). Direct HTTP via reqwest.
- `tuner` — AI-assisted tuning orchestrator (configurable loop: context → LLM → safety → apply → replay → compare)
- `tuner::types` — Recommendation, TuningConfig, TuningIteration, TuningReport
- `tuner::context` — PG introspection (pg_settings, schema, pg_stat_statements, EXPLAIN plans)
- `tuner::advisor` — TuningAdvisor trait with Claude/OpenAI/Ollama providers
- `tuner::safety` — Parameter allowlist, blocked operations, production hostname check
- `tuner::apply` — Recommendation application with rollback tracking
- `cli` — Clap derive-based CLI argument structs (11 subcommands: capture, replay, compare, inspect, proxy, run, ab, web, transform, tune)
- `web` — Axum HTTP server + WebSocket dashboard (embedded static files via rust-embed, SQLite metadata via rusqlite)
- `web::db` — SQLite schema + CRUD for workloads, runs, proxy_sessions, threshold_results
- `web::state` — `AppState` (db, data_dir, ws_broadcast, task_manager)
- `web::tasks` — `TaskManager` for background ops (proxy, replay, pipeline) with CancellationToken
- `web::ws` — WebSocket handler with `WsMessage` enum (ProxyStarted, ReplayProgress, PipelineCompleted, etc.)
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

## Milestone Status

- **M1: Capture & Replay** — Complete (with gap closure). CSV log capture, proxy capture, transaction boundaries, PII masking, async replay with transaction-aware error handling, comparison reports with exit codes.
- **M2: Scaled Benchmark** — Complete. Workload classification (Analytical/Transactional/Mixed/Bulk), session scaling with stagger, per-category scaling (`--scale-analytical 2 --scale-transactional 4`), capacity planning reports.
- **M3: CI/CD Integration** — Complete. TOML config (`pg-retest run --config .pg-retest.toml`), Docker provisioner via CLI subprocess, JUnit XML output, threshold evaluation, pipeline orchestrator with staged exit codes (0-5), A/B variant mode via `[[variants]]` in pipeline config.
- **M4: Cross-Database Capture (MySQL)** — Complete. MySQL slow query log parser, composable SQL transform pipeline (regex-based: backticks, LIMIT, IFNULL, IF, UNIX_TIMESTAMP), CLI `--source-type mysql-slow`, pipeline config support.
- **Gap Closure** — Complete. Per-category scaling, A/B variant testing (`pg-retest ab`), cloud-native capture from AWS RDS/Aurora (`--source-type rds`).
- **Web Dashboard** — Complete. Axum + Alpine.js + Chart.js SPA (`pg-retest web --port 8080`). 11 pages: dashboard, workloads, proxy, replay, A/B, compare, pipeline, history, transform, tuning, help. WebSocket real-time updates. SQLite metadata storage.
- **Workload Transform** — Complete. AI-powered workload transformation (`pg-retest transform analyze|plan|apply`). 3-layer architecture: deterministic Analyzer (Union-Find table grouping), multi-provider LLM Planner (Claude/OpenAI/Ollama), deterministic Engine (weighted session duplication, query injection, group removal). TOML transform plans as intermediate artifact. Design at `docs/plans/2026-03-07-workload-transform-design.md`. 201 tests.
- **M5: AI-Assisted Tuning** — Complete. Multi-provider LLM tuning (`pg-retest tune`). Configurable loop: collect PG context → LLM recommendations → safety validation → apply → replay → compare → iterate. 4 recommendation types (config, index, query rewrite, schema). Safety allowlist (~41 safe PG params), production hostname check. Claude/OpenAI/Ollama providers. Dry-run default. Web dashboard tuning page. Design at `docs/plans/2026-03-07-m5-ai-tuning-design-v2.md`. 215 tests.

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
- Transform: `extract_tables()` groups tables by co-occurrence within a single SQL statement (Union-Find), not by session co-occurrence. Two tables in separate queries won't be grouped unless a third query touches both.
- Transform: The engine's Remove operation uses `query_indices` from the plan's group definition to identify which queries to remove. Empty `query_indices` means nothing is removed.
- Transform: Transform plans are TOML files with `#[serde(tag = "type")]` on `TransformRule` enum. The `type` field must be `scale`, `inject`, `inject_session`, or `remove`.
- Transform: `apply_transform()` is deterministic when given the same seed. Default seed is derived from the plan's prompt string hash.
- Transform: LLM planner uses `reqwest` for direct HTTP calls (no SDK dependencies). API key resolution: `--api-key` flag → `ANTHROPIC_API_KEY` env → `OPENAI_API_KEY` env.
- Transform: `--dry-run` on `transform plan` shows the analyzer output and system prompt without calling the LLM API.

## Conventions

- Target PostgreSQL as the primary replay destination for all milestones.
- Workload profiles should be a portable, version-stamped format (not tied to a specific PG version).
- Capture and replay must be decoupled — capture produces a profile file; replay consumes it. They should never require simultaneous access to source and target.
- Connection-level parallelism in replay is critical for realistic results; avoid serializing inherently parallel workloads.
- Configuration changes and server differences are the variables under test — the tool itself should introduce minimal overhead or variance.
