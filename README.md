# pg-retest

Capture, replay, and compare PostgreSQL workloads.

pg-retest captures SQL workload from PostgreSQL server logs, replays it against a target database, and produces a side-by-side performance comparison report. Use it to validate configuration changes, server migrations, and capacity planning.

## Features

- **Capture** workload from PG CSV logs with transaction boundary tracking
- **PII masking** — strip string and numeric literals from captured SQL
- **Replay** against any PostgreSQL target with per-connection parallelism
- **Transaction-aware replay** — auto-rollback on failure, skip remaining queries in failed transaction
- **Read-only mode** — strip DML for safe replay against production replicas
- **Speed control** — compress or stretch timing between queries
- **Scaled benchmark** — duplicate sessions N times with staggered offsets for load testing
- **Workload classification** — categorize as Analytical, Transactional, Mixed, or Bulk
- **Comparison reports** — per-query latency regression detection with exit codes for CI
- **Capacity planning** — throughput QPS, latency percentiles, error rates at scale

## Quick Start

```bash
# Build
cargo build --release

# 1. Capture workload from PG CSV logs
pg-retest capture --source-log /path/to/postgresql.csv --output workload.wkl

# 2. Replay against target database
pg-retest replay --workload workload.wkl --target "host=localhost dbname=mydb user=postgres"

# 3. Compare results
pg-retest compare --source workload.wkl --replay results.wkl --json report.json

# Inspect a workload profile
pg-retest inspect workload.wkl
```

## PostgreSQL Logging Setup

pg-retest captures workload by parsing PostgreSQL CSV logs. You need to configure your PostgreSQL server to produce these logs.

### Check Current Settings

Connect to your PostgreSQL server and check if logging is already configured:

```sql
SHOW logging_collector;              -- Must be 'on'
SHOW log_destination;                -- Must include 'csvlog'
SHOW log_min_duration_statement;     -- Check current value
```

### Configure Logging

Add or modify these settings in `postgresql.conf`:

```ini
# Required: enables the log file collector process
# NOTE: changing this requires a PostgreSQL RESTART if not already 'on'
logging_collector = on

# Required: enable CSV log output
# Change takes effect after: pg_ctl reload (no restart needed)
log_destination = 'csvlog'

# Recommended: log all statements with their duration in one line
# Change takes effect after: pg_ctl reload (no restart needed)
log_min_duration_statement = 0    # logs every statement with duration

# Alternative: log statements and duration separately
# log_statement = 'all'           # logs all SQL statements
# log_duration = on               # logs duration of each statement

# Optional: useful log file naming
log_filename = 'postgresql-%Y-%m-%d.log'
log_rotation_age = 1d
```

### Which settings require a restart?

| Setting | Restart required? | Notes |
|---------|:-:|-------|
| `log_statement = 'all'` | No | `ALTER SYSTEM` + `SELECT pg_reload_conf()` |
| `log_duration = on` | No | Reload only |
| `log_destination = 'csvlog'` | No | Reload only |
| `log_min_duration_statement = 0` | No | Reload only |
| `logging_collector = on` | **Yes** | Only if not already enabled (usually is in production) |

### Apply Changes

If `logging_collector` was already `on`:

```bash
# No restart needed — just reload config
pg_ctl reload -D /path/to/data
# OR from SQL:
# SELECT pg_reload_conf();
```

If `logging_collector` was `off` (requires restart):

```bash
pg_ctl restart -D /path/to/data
```

### Verify Logging

After applying changes, run a few queries and check that CSV logs appear:

```bash
ls -la /path/to/data/log/*.csv
# You should see files like: postgresql-2024-03-08.csv
```

### Log File Location

- **Default:** `$PGDATA/log/` directory
- **Custom:** Set `log_directory` in postgresql.conf
- **RDS/Aurora:** Download logs via AWS Console, CLI, or RDS API
- **Cloud SQL:** Access via Google Cloud Console or gcloud CLI
- **Azure:** Access via Azure Portal or az CLI

### Performance Impact

Logging with `log_min_duration_statement = 0` has minimal overhead on most workloads (typically <2% throughput impact). For extremely high-throughput systems (>50k queries/sec), consider:

- Setting `log_min_duration_statement = 1` to skip sub-millisecond queries
- Capturing during off-peak windows
- Using `log_statement = 'none'` and relying on `auto_explain` instead

## Capture Options

```bash
pg-retest capture \
  --source-log /path/to/postgresql.csv \
  --output workload.wkl \
  --source-host prod-db-01 \
  --pg-version 16.2 \
  --mask-values              # Strip PII from SQL literals
```

### PII Masking

The `--mask-values` flag replaces string literals with `$S` and numeric literals with `$N`:

```
-- Original:
SELECT * FROM users WHERE email = 'alice@corp.com' AND id = 42

-- Masked:
SELECT * FROM users WHERE email = $S AND id = $N
```

Masking handles SQL edge cases: escaped quotes (`''`), dollar-quoted strings (`$$...$$`), and preserves numbers in identifiers (`table3`, `col1`).

## Replay Modes

### Read-Write (default)

Replays all captured queries including INSERT, UPDATE, DELETE. **Important:** use a backup or snapshot of your database — DML will modify data.

```bash
pg-retest replay --workload workload.wkl --target "host=localhost dbname=mydb user=postgres"
```

### Read-Only

Strips all DML (INSERT, UPDATE, DELETE), DDL (CREATE, ALTER, DROP), and transaction control (BEGIN, COMMIT, ROLLBACK), replaying only SELECT queries. Safe to run against production data.

```bash
pg-retest replay --workload workload.wkl --target "..." --read-only
```

### Speed Control

Compress or stretch timing gaps between queries:

```bash
# 2x faster (halves wait times between queries)
pg-retest replay --workload workload.wkl --target "..." --speed 2.0

# Half speed (doubles wait times)
pg-retest replay --workload workload.wkl --target "..." --speed 0.5
```

### Scaled Benchmark

Duplicate sessions N times for load testing:

```bash
# 4x the original sessions, staggered 500ms apart
pg-retest replay --workload workload.wkl --target "..." --scale 4 --stagger-ms 500
```

This produces a capacity planning report with throughput (queries/sec), latency percentiles, and error rates.

## Transaction Support

pg-retest tracks transaction boundaries (BEGIN/COMMIT/ROLLBACK) during capture and provides transaction-aware replay:

- Queries within a transaction share a `transaction_id`
- If a query inside a transaction fails, the replay engine automatically issues a ROLLBACK and skips remaining queries in that transaction
- COMMIT for a failed transaction is converted to a no-op

## Comparison Report

The compare command produces a terminal summary and optional JSON report:

```bash
pg-retest compare --source workload.wkl --replay results.wkl --json report.json --threshold 20
```

- `--threshold`: Flag queries that are slower by this percentage (default: 20%)
- `--fail-on-regression`: Exit with code 1 if regressions are detected
- `--fail-on-error`: Exit with code 2 if query errors occurred

### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | PASS — all checks passed |
| 1 | FAIL — regressions detected (with `--fail-on-regression`) |
| 2 | FAIL — query errors detected (with `--fail-on-error`) |

When both flags are set, errors take priority over regressions.

### Report Metrics

| Metric | Description |
|--------|-------------|
| Total queries | Count of queries in source vs. replay |
| Avg/P50/P95/P99 latency | Latency percentiles (microseconds) |
| Errors | Queries that failed during replay |
| Regressions | Individual queries exceeding the threshold |

## Workload Classification

Classify captured workloads to understand their characteristics:

```bash
pg-retest inspect workload.wkl --classify
```

| Class | Criteria |
|-------|---------|
| **Analytical** | >80% reads, avg latency >10ms (OLAP pattern) |
| **Transactional** | >20% writes, avg latency <5ms, >2 transactions (OLTP pattern) |
| **Bulk** | >80% writes, <=2 transactions (data loading) |
| **Mixed** | Everything else |

Classification outputs per-session breakdown with read/write percentages, average latency, and transaction count.

## Workload Profile Format

Profiles are stored as MessagePack binary files (`.wkl`, v2 format). Use `inspect` to view as JSON:

```bash
pg-retest inspect workload.wkl | jq .
```

v2 profiles include transaction IDs on queries. v1 profiles (without transaction support) are fully backward compatible.

## Building

```bash
# Debug build
cargo build

# Release build (optimized)
cargo build --release

# Run tests
cargo test

# Run a single test file
cargo test --test profile_io_test

# Run a single test function
cargo test --test compare_test test_comparison_regressions

# Run with verbose logging
RUST_LOG=debug pg-retest capture --source-log ...

# Lint
cargo clippy

# Format
cargo fmt
```
