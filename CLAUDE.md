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
- `replay::scaling` — Session duplication with staggered offsets for load testing
- `classify` — Workload classification (Analytical/Transactional/Mixed/Bulk) based on read/write ratio, latency, transaction count
- `compare` — Performance comparison logic + terminal/JSON reporting + exit code evaluation
- `compare::capacity` — Scaled replay reporting (throughput QPS, latency percentiles, error rate)
- `cli` — Clap derive-based CLI argument structs

## Milestone Status

- **M1: Capture & Replay** — Complete (with gap closure). CSV log capture, transaction boundaries, PII masking, async replay with transaction-aware error handling, comparison reports with exit codes. 1725 LOC, 59 tests.
- **M2: Scaled Benchmark** — Complete. Workload classification (Analytical/Transactional/Mixed/Bulk), session scaling with stagger, capacity planning reports.
- **M3: CI/CD Integration** — Design complete (`docs/plans/2026-03-04-m3-cicd-design.md`). TOML config, Docker provisioner, JUnit XML output, pipeline orchestrator.
- **M4: Cross-Database Capture** — Design complete (`docs/plans/2026-03-04-m4-mysql-capture-design.md`). MySQL slow/general log parsers, SQL transform pipeline.
- **M5: AI-Assisted Tuning** — Design complete (`docs/plans/2026-03-04-m5-ai-tuning-design.md`). Claude API integration, tuning loop, A/B variants.

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

## Conventions

- Target PostgreSQL as the primary replay destination for all milestones.
- Workload profiles should be a portable, version-stamped format (not tied to a specific PG version).
- Capture and replay must be decoupled — capture produces a profile file; replay consumes it. They should never require simultaneous access to source and target.
- Connection-level parallelism in replay is critical for realistic results; avoid serializing inherently parallel workloads.
- Configuration changes and server differences are the variables under test — the tool itself should introduce minimal overhead or variance.
