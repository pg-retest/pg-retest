# The Golden Path: End-to-End pg-retest Demo

This guide walks through the complete pg-retest workflow — from capturing
production traffic to running a synthetic benchmark. Every step uses the CLI.

By the end you will have:
- Captured real database traffic through the proxy
- Replayed it against a restored target and compared results
- Verified data correctness with a drift check
- Generated a synthetic workload with fixed IDs and matching data
- Run a repeatable benchmark against a clean target

---

## Prerequisites

- pg-retest built and on your PATH:
  ```bash
  cargo build --release
  export PATH="$PWD/target/release:$PATH"
  ```
- Two PostgreSQL databases: **source** (production-like, receiving traffic) and **target** (for replay)
- Python 3.8+ with `psycopg2-binary` and `msgpack`:
  ```bash
  pip install psycopg2-binary msgpack
  ```
- Backup/restore capability (pg_dump/pg_restore, pgBackRest, or cloud PITR)

## The Scenario

You have a production PostgreSQL database running an e-commerce application.
You want to:

1. Capture real traffic without disrupting production
2. Replay it against a test target to validate a migration or upgrade
3. Generate a synthetic benchmark for repeatable, deterministic testing

---

## Step 1: Traffic Is Already Flowing

Your application is running normally — web servers, background workers, cron
jobs — all hitting the source database. You do not need to stop anything.
pg-retest captures traffic transparently through a proxy that sits in the
connection path.

```
  App Servers ──► PostgreSQL (source-db:5432)
```

## Step 2: Take a Backup

Before you start capturing, ensure you have a way to restore the source
database to its current state. The proxy will auto-create a named restore
point when capture starts (Step 5), but the underlying backup infrastructure
must already be in place.

**Self-hosted with WAL archiving (recommended):**

```sql
-- Verify WAL archiving is enabled
SHOW wal_level;        -- Must be 'replica' or 'logical'
SHOW archive_mode;     -- Must be 'on'
```

With WAL archiving enabled, pg-retest's auto-created restore point gives you
a named PITR target. No manual `pg_dump` needed.

**Development / Docker / no WAL archiving:**

```bash
pg_dump -Fc -h source-db -U myuser -d myapp > pre-capture.dump
```

This dump is your fallback. You will restore from it before each replay.

## Step 3: Start the Proxy

Start the proxy in persistent mode. It stays running indefinitely — capture
is toggled on and off separately via `proxy-ctl`.

```bash
pg-retest proxy \
  --listen 0.0.0.0:5433 \
  --target source-db:5432 \
  --id-mode full \
  --source-db "host=source-db dbname=myapp user=myuser password=secret" \
  --id-capture-implicit \
  --persistent
```

Key flags:
- `--persistent` — proxy stays up; capture is controlled via `proxy-ctl`
- `--id-mode full` — snapshot sequences + capture RETURNING values for write-heavy workloads
- `--source-db` — connection string for sequence snapshot (required with `--id-mode full`)
- `--id-capture-implicit` — auto-inject RETURNING on bare INSERTs so the proxy can track generated IDs without application changes (stealth mode: clients see normal responses)

The control port defaults to 9091. Check that the proxy is up:

```bash
pg-retest proxy-ctl status --proxy localhost:9091
```

## Step 4: Route Traffic Through the Proxy

Redirect some or all of your application traffic through the proxy. Two
options:

**Option A: Load balancer insertion (recommended for production)**

Add `proxy-host:5433` to your existing load balancer pool alongside the
direct database endpoint. Even routing 1-2 backends through the proxy gives
you a representative sample.

**Option B: Direct connection change**

Update your application's connection string to point at the proxy:

```
# Before
host=source-db port=5432 dbname=myapp user=myuser

# After
host=proxy-host port=5433 dbname=myapp user=myuser
```

Verify traffic is flowing:

```bash
pg-retest proxy-ctl status --proxy localhost:9091
```

You should see active connections. The proxy adds approximately 0.1ms latency
per query — negligible for virtually all workloads.

## Step 5: Start Capture

```bash
pg-retest proxy-ctl start-capture --proxy localhost:9091
```

**Watch the log output.** When capture starts, pg-retest automatically creates
a PostgreSQL restore point:

```
INFO  Created restore point: pg_retest_capture_20260326_143000
```

**Write down this restore point name.** It is your PITR target — the exact
moment capture began. You will restore to this point before replaying.

> **Prerequisite:** The database user needs the `pg_create_restore_point`
> privilege. Superusers have it by default. For a non-superuser:
> ```sql
> GRANT EXECUTE ON FUNCTION pg_create_restore_point(text) TO myuser;
> ```

## Step 6: Monitor Capture

While capture is running, check status to see queries flowing through:

```bash
pg-retest proxy-ctl status --proxy localhost:9091
```

This shows:
- Capture state (active/stopped)
- Number of queries captured
- Active sessions
- Approximate QPS

Let it run for your desired duration. Typical durations:

| Use Case | Duration |
|---|---|
| Quick validation | 60 seconds |
| Representative workload | 5-15 minutes |
| Full traffic cycle | 1-4 hours |

## Step 7: Stop Capture

```bash
pg-retest proxy-ctl stop-capture --proxy localhost:9091 --output workload.wkl
```

The `.wkl` file is written to disk. The proxy stays running — you can start
another capture later without restarting it.

If you are done capturing, you can optionally remove the proxy from your load
balancer rotation. Or leave it in place for future captures — with capture
stopped, overhead is near-zero.

## Step 8: Inspect the Workload

Get a summary of what was captured:

```bash
pg-retest inspect workload.wkl
```

This shows metadata (source host, capture method, timestamp), session count,
query count, and duration.

Add `--classify` to see the workload breakdown by category:

```bash
pg-retest inspect workload.wkl --classify
```

This categorizes each session as Analytical, Transactional, Mixed, or Bulk —
useful for understanding the workload mix and for per-category scaling later.

For JSON output (useful for scripting):

```bash
pg-retest inspect workload.wkl --output-format json | jq .
```

## Step 9: Compile for Deterministic Replay

The captured workload contains `response_values` — the actual RETURNING
results from the source database during capture. The `compile` command
pre-resolves all ID references into the SQL text, producing a workload that
replays deterministically without needing `--id-mode` at replay time.

```bash
pg-retest compile workload.wkl -o workload-compiled.wkl
```

The compiled workload:
- Has all sequence-dependent IDs baked into the SQL
- Does not require `--id-mode` during replay
- Is ideal for CI/CD pipelines and repeated replay iterations
- Works against any PITR-restored target at the capture-start restore point

## Step 10: Restore Backup to Target

Restore the target database to the exact state of the source at capture start.
Use the restore point name from Step 5.

**pgBackRest / WAL archive:**

```bash
# In recovery.conf or postgresql.auto.conf on the target:
# recovery_target_name = 'pg_retest_capture_20260326_143000'
# recovery_target_action = 'promote'

pgbackrest restore \
  --stanza=myapp \
  --target="pg_retest_capture_20260326_143000" \
  --target-action=promote \
  --pg1-path=/var/lib/postgresql/data
```

**AWS RDS (point-in-time restore):**

```bash
aws rds restore-db-instance-to-point-in-time \
  --source-db-instance-identifier myapp-prod \
  --target-db-instance-identifier myapp-replay-target \
  --restore-time "2026-03-26T14:30:00Z"
```

**pg_dump / pg_restore (development):**

```bash
pg_restore -h target-db -U myuser -d myapp --clean --if-exists pre-capture.dump
```

After restore, verify the target is accessible:

```bash
psql "host=target-db dbname=myapp user=myuser" -c "SELECT count(*) FROM orders;"
```

## Step 11: Replay

Replay the compiled workload against the restored target:

```bash
pg-retest replay \
  --workload workload-compiled.wkl \
  --target "host=target-db dbname=myapp user=myuser password=secret" \
  -o results.wkl
```

The replay engine:
- Opens parallel connections matching the original session count
- Preserves inter-query timing (use `--speed 2.0` to compress)
- Tracks transaction boundaries (auto-rollback on mid-transaction failure)
- Writes results (with actual replay timing) to `results.wkl`

## Step 12: Compare

Compare the original capture timing against the replay results:

```bash
pg-retest compare --source workload.wkl --replay results.wkl
```

This prints a terminal report with:
- Total queries (source vs. replay)
- Latency percentiles (avg, p50, p95, p99)
- Error count and rate
- Individual query regressions exceeding the threshold

For JSON output:

```bash
pg-retest compare \
  --source workload.wkl \
  --replay results.wkl \
  --output-format json
```

To fail the command on regressions (useful for CI):

```bash
pg-retest compare \
  --source workload.wkl \
  --replay results.wkl \
  --fail-on-regression \
  --threshold 20
```

Exit code 0 = pass, 1 = regressions detected, 2 = errors detected.

## Step 13: Drift Check

Verify that the replay produced the same data state as the source. The drift
check compares every table between two databases — row counts, checksums, and
sample mismatches.

```bash
python3 demo/drift-check.py \
  --db-a "host=source-db dbname=myapp user=myuser password=secret" \
  --db-b "host=target-db dbname=myapp user=myuser password=secret"
```

For CI pipelines, add `--strict` to exit with code 1 if any drift is found:

```bash
python3 demo/drift-check.py --strict \
  --db-a "host=source-db dbname=myapp user=myuser password=secret" \
  --db-b "host=target-db dbname=myapp user=myuser password=secret"
```

> **Note:** Some drift is expected for write workloads. Concurrent session
> ordering during replay is approximate — timestamps, UUIDs, and some
> sequence-assigned IDs will differ. The drift check tells you *how much*
> divergence occurred. See [Replay Accuracy](replay-accuracy.md) for
> detailed benchmarks.

## Step 14: Generate Synthetic Workload

The synthesizer analyzes your captured workload's statistical fingerprint and
produces two artifacts:

1. A new `.wkl` file with the same query patterns but all IDs are fixed
   literals (no sequence/UUID dependencies)
2. A matching SQL data file containing exactly the rows the workload references

```bash
python3 demo/synthesize-workload.py \
  --input workload.wkl \
  --source-db "host=source-db dbname=myapp user=myuser password=secret" \
  --output-workload synthetic.wkl \
  --output-data synthetic-data.sql \
  --seed 42
```

The `--seed` flag makes the output deterministic — same seed produces
identical workload and data files. This is what makes the synthetic benchmark
repeatable.

The synthetic workload preserves:
- Same query template distribution (SELECT/INSERT/UPDATE/DELETE mix)
- Same session structure and concurrency patterns
- Same timing distribution between queries
- Same table relationships and join patterns

But replaces all runtime-generated values (sequences, UUIDs, timestamps) with
pre-computed literals that match the synthetic data file.

## Step 15: Load Synthetic Data

Prepare a fresh target database and load the synthetic data:

```bash
# Start from a clean database (or restore and wipe)
psql "host=target-db dbname=myapp user=myuser password=secret" \
  < synthetic-data.sql
```

The synthetic data file includes:
- Table creation (in FK-dependency order)
- Row data matching the workload's ID references
- Sequence resets so IDs align
- Indexes and `ANALYZE` for realistic query plans

## Step 16: Replay Synthetic Benchmark

```bash
pg-retest replay \
  --workload synthetic.wkl \
  --target "host=target-db dbname=myapp user=myuser password=secret" \
  -o synthetic-results.wkl
```

Because all IDs are pre-resolved literals, this replay needs no `--id-mode`
flag and produces zero ID-related errors. It is a clean, repeatable
benchmark.

## Step 17: Compare Synthetic Results

```bash
pg-retest compare --source synthetic.wkl --replay synthetic-results.wkl
```

This gives you the performance profile of the synthetic benchmark. Use it as
a baseline for iterative testing — the synthetic workload and data never
change, so any performance difference comes from the target configuration.

---

## Iterating

The real power of pg-retest is the iteration loop. Once you have a workload
(captured or synthetic), each iteration takes minutes:

```
Restore backup ──► Make a change ──► Replay ──► Compare ──► Repeat
```

### Example: Testing a Configuration Change

```bash
# 1. Restore target to clean state
pg_restore -h target-db -U myuser -d myapp --clean --if-exists pre-capture.dump

# 2. Apply configuration change
psql -h target-db -d myapp -c "ALTER SYSTEM SET work_mem = '256MB';"
psql -h target-db -d myapp -c "SELECT pg_reload_conf();"

# 3. Replay
pg-retest replay \
  --workload workload-compiled.wkl \
  --target "host=target-db dbname=myapp user=myuser password=secret" \
  -o results-tuned.wkl

# 4. Compare against baseline
pg-retest compare --source workload.wkl --replay results-tuned.wkl
```

### Example: Capacity Planning at 5x Scale

```bash
pg-retest replay \
  --workload workload-compiled.wkl \
  --target "host=target-db dbname=myapp user=myuser password=secret" \
  --scale 5 \
  --stagger-ms 200 \
  -o results-5x.wkl
```

### Example: A/B Testing Two Targets

```bash
pg-retest ab \
  --workload workload-compiled.wkl \
  --variant "pg15=host=db-pg15 dbname=myapp user=myuser password=secret" \
  --variant "pg16=host=db-pg16 dbname=myapp user=myuser password=secret" \
  --json ab-report.json
```

### Example: AI-Assisted Tuning

```bash
# Dry-run first (default) — see recommendations without applying
pg-retest tune \
  --workload workload-compiled.wkl \
  --target "host=target-db dbname=myapp user=myuser password=secret" \
  --provider claude \
  --max-iterations 3

# Then apply with --apply (auto-rollbacks on p95 regression)
pg-retest tune \
  --workload workload-compiled.wkl \
  --target "host=target-db dbname=myapp user=myuser password=secret" \
  --provider claude \
  --apply
```

---

## Quick Reference: The Full Command Sequence

```bash
# --- Capture phase ---
pg-retest proxy --listen 0.0.0.0:5433 --target source-db:5432 \
  --id-mode full --source-db "host=source-db ..." --id-capture-implicit --persistent
pg-retest proxy-ctl start-capture --proxy localhost:9091
# ... wait for desired duration ...
pg-retest proxy-ctl stop-capture --proxy localhost:9091 --output workload.wkl

# --- Inspect & compile ---
pg-retest inspect workload.wkl --classify
pg-retest compile workload.wkl -o workload-compiled.wkl

# --- Restore & replay ---
pg_restore -h target-db -U myuser -d myapp --clean --if-exists pre-capture.dump
pg-retest replay --workload workload-compiled.wkl \
  --target "host=target-db ..." -o results.wkl
pg-retest compare --source workload.wkl --replay results.wkl

# --- Drift check ---
python3 demo/drift-check.py --db-a "host=source-db ..." --db-b "host=target-db ..."

# --- Synthetic benchmark ---
python3 demo/synthesize-workload.py --input workload.wkl \
  --source-db "host=source-db ..." --output-workload synthetic.wkl \
  --output-data synthetic-data.sql --seed 42
psql "host=target-db ..." < synthetic-data.sql
pg-retest replay --workload synthetic.wkl --target "host=target-db ..." \
  -o synthetic-results.wkl
pg-retest compare --source synthetic.wkl --replay synthetic-results.wkl
```

---

## Further Reading

- [Production Workflow Guide](production-workflow.md) — infrastructure setup, PITR restore, security
- [ID Correlation](id-correlation.md) — detailed ID handling modes and driver compatibility
- [Replay Accuracy](replay-accuracy.md) — per-mode benchmarks and fidelity analysis
- [Synthetic Data](synthetic-data.md) — synthetic data generation details
- [Docker Demo](demo.md) — one-command demo with pre-seeded databases
