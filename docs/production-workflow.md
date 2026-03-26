# Production Workflow Guide

This guide covers the practical steps for using pg-retest against real production PostgreSQL databases. It assumes you have pg-retest installed and are familiar with the basic capture/replay/compare cycle from the [README](../README.md).

> **New to pg-retest?** Start with the [Golden Path Demo Guide](golden-path-demo.md) for a complete end-to-end CLI walkthrough — from capturing traffic to running a synthetic benchmark.

## The Key Insight

You never stop production to capture. The pg-retest proxy slides into your infrastructure transparently -- applications keep running, users see no difference, and you get a complete recording of real traffic.

## Infrastructure Setup

### Deploying the Proxy

The pg-retest proxy is a PostgreSQL wire protocol proxy that sits between your application and database. There are two deployment strategies:

**Option A: Load balancer insertion (recommended)**

Add the proxy to 1-2 endpoints in your existing load balancer or DNS round-robin. Applications connect to the proxy naturally -- no application configuration changes needed.

```
                    ┌─────────────┐
                    │  App Server  │
                    └──────┬──────┘
                           │
                    ┌──────▼──────┐
                    │ Load Balancer│
                    └──┬──────┬───┘
                       │      │
              ┌────────▼┐  ┌──▼────────┐
              │ PG Direct│  │ pg-retest │
              │ :5432    │  │ proxy     │
              └──────────┘  │ :5433     │
                            └─────┬────┘
                                  │
                            ┌─────▼────┐
                            │ PG Server│
                            │ :5432    │
                            └──────────┘
```

**Option B: Direct proxy insertion**

Point your application's connection string at the proxy. This requires a config change but gives you full traffic coverage.

```bash
# Start the persistent proxy
pg-retest proxy \
  --listen 0.0.0.0:5433 \
  --target prod-db:5432 \
  --persistent
```

### Connection Pooler Placement

If you use PgBouncer, Pgpool-II, or another connection pooler, place the proxy **between the pooler and PostgreSQL**, not between the application and the pooler.

```
  App  →  PgBouncer  →  pg-retest proxy  →  PostgreSQL
                         (captures here)
```

Placing the proxy before the pooler would capture the pooler's connection management traffic (connection reuse, session teardown) rather than the application's actual SQL workload. It would also interfere with the pooler's connection multiplexing.

### Security Considerations

The proxy sees all traffic including authentication credentials. Run it within your trusted network perimeter -- the same network segment where your database and application servers live. Do not expose the proxy port to untrusted networks.

For TLS between the proxy and the database, use `--tls-mode require`:

```bash
pg-retest proxy \
  --listen 0.0.0.0:5433 \
  --target prod-db:5432 \
  --persistent \
  --tls-mode require \
  --tls-ca-cert /path/to/ca.crt
```

## Capture Process

### Step 1: Start Capture

With a persistent proxy already running, start capture from a separate terminal or the web dashboard:

```bash
# Via proxy-ctl
pg-retest proxy-ctl start-capture --proxy localhost:9091

# Or via the web dashboard: Proxy page → Start Capture
```

For a one-shot capture (non-persistent mode):

```bash
pg-retest proxy \
  --listen 0.0.0.0:5433 \
  --target prod-db:5432 \
  --output workload.wkl \
  --id-mode full \
  --source-db "host=prod-db port=5432 dbname=myapp user=myuser password=mypass" \
  --duration 5m
```

### Step 2: Restore Point

When capture starts, pg-retest automatically records a PostgreSQL restore point:

```sql
SELECT pg_create_restore_point('pg_retest_capture_20260325_143000');
```

**Note this timestamp.** It is your PITR target -- the exact moment your capture began. You will restore to this point before replaying.

**Prerequisite:** The database user needs the `pg_create_restore_point` privilege:

```sql
-- Superuser already has this. For a non-superuser:
GRANT EXECUTE ON FUNCTION pg_create_restore_point(text) TO myuser;
```

### Step 3: Let It Run

Capture runs for your desired duration. The proxy adds approximately 0.1ms latency per query -- negligible for virtually all workloads. Typical capture durations:

| Use Case | Duration | Notes |
|---|---|---|
| Quick validation | 60 seconds | Enough for basic regression testing |
| Representative workload | 5-15 minutes | Covers most query patterns |
| Full traffic cycle | 1-4 hours | Captures batch jobs, cron tasks, usage peaks |
| Peak load capture | During peak hours | Best for capacity planning scenarios |

### Step 4: Stop Capture

```bash
# Persistent mode
pg-retest proxy-ctl stop-capture --proxy localhost:9091 --output workload.wkl

# One-shot mode: Ctrl+C or wait for --duration to expire
```

The `.wkl` file is written on stop. For persistent mode, you can start another capture later without restarting the proxy.

### Step 5: Remove or Keep the Proxy

After capture, either remove the proxy from your load balancer rotation or leave it in place for future captures. The persistent proxy with `--no-capture` mode has near-zero overhead when not actively capturing.

## PITR Restore

The restored database must be an exact copy of production at the moment capture started. This is what makes replay results meaningful -- the target database's data state matches the queries' expectations.

### Prerequisites

Your source database must have WAL archiving enabled:

```sql
-- Check current settings:
SHOW wal_level;          -- Must be 'replica' or 'logical'
SHOW archive_mode;       -- Must be 'on'
SHOW archive_command;    -- Must be configured
```

### Restore Methods by Environment

| Environment | Tool | Restore Command |
|---|---|---|
| Self-hosted | pgBackRest | `pgbackrest restore --target="TIMESTAMP" --target-action=promote` |
| Self-hosted | Barman | `barman recover --target-time "TIMESTAMP"` |
| Self-hosted | pg_basebackup + WAL | Manual `recovery.conf` with `recovery_target_name = 'pg_retest_capture_YYYYMMDD_HHMMSS'` |
| AWS RDS | Snapshot | `aws rds restore-db-instance-to-point-in-time --restore-time TIMESTAMP` |
| AWS Aurora | Snapshot | `aws rds restore-db-cluster-to-point-in-time --restore-to-time TIMESTAMP` |
| GCP Cloud SQL | Backup | `gcloud sql instances clone --point-in-time TIMESTAMP` |
| Azure PG | Geo-restore | Portal -> Point-in-time restore |
| Docker/dev | pg_dump/pg_restore | `pg_dump` before capture, `pg_restore` before each replay |

**Using the restore point name** (pgBackRest / WAL archive):

```ini
# recovery.conf or postgresql.auto.conf
recovery_target_name = 'pg_retest_capture_20260325_143000'
recovery_target_action = 'promote'
```

**Using the timestamp** (RDS, Aurora, Cloud SQL):

Use the timestamp from the restore point creation. The pg-retest log output includes this:

```
INFO  Created restore point: pg_retest_capture_20260325_143000
```

### After Restore

Once the restore completes:

1. Verify the restored database is accessible and data looks correct
2. **Take a full backup of this restored copy** -- you will reuse it for iterative testing
3. This backup becomes your "clean slate" for every replay iteration

## Iterative Testing Loop

This is where pg-retest pays off. Once you have a captured workload and a backup of the restored database, you can iterate rapidly:

```
Restore backup → Replay workload → Analyze results →
  Tweak config/schema → Restore backup → Replay again → Compare → ...
```

### Example: Testing a Configuration Change

```bash
# 1. Restore from your PITR backup
pg_restore -d testdb clean_backup.dump

# 2. Replay the captured workload (baseline)
pg-retest replay \
  --workload workload.wkl \
  --target "host=test-db dbname=testdb user=postgres" \
  --output baseline.wkl \
  --id-mode full

# 3. Restore again (clean slate)
pg_restore -d testdb clean_backup.dump

# 4. Apply your configuration change
psql -h test-db -d testdb -c "ALTER SYSTEM SET work_mem = '256MB';"
psql -h test-db -d testdb -c "SELECT pg_reload_conf();"

# 5. Replay again
pg-retest replay \
  --workload workload.wkl \
  --target "host=test-db dbname=testdb user=postgres" \
  --output tuned.wkl \
  --id-mode full

# 6. Compare
pg-retest compare \
  --source baseline.wkl \
  --replay tuned.wkl \
  --fail-on-regression
```

### Example: A/B Testing Two Targets

```bash
pg-retest ab \
  --workload workload.wkl \
  --variant "pg15=host=db-pg15 dbname=myapp user=postgres" \
  --variant "pg16=host=db-pg16 dbname=myapp user=postgres" \
  --id-mode full \
  --json ab-report.json
```

### Example: Capacity Planning

```bash
# Replay at 5x scale to find breaking points
pg-retest replay \
  --workload workload.wkl \
  --target "host=test-db dbname=testdb user=postgres" \
  --output scaled-5x.wkl \
  --scale 5 \
  --stagger-ms 200 \
  --id-mode full
```

## Replay Is Simulation, Not Replication

pg-retest replay is a high-fidelity **simulation** of your production traffic. It is not byte-identical replication. Even with `--id-mode full`:

- **Concurrent session ordering is approximate.** Replay preserves timing offsets but OS scheduling and network jitter mean the exact interleaving of concurrent sessions will differ slightly.
- **Non-deterministic functions produce different values.** `now()`, `random()`, `clock_timestamp()`, `gen_random_uuid()` execute at replay time, not at the original production time.
- **Data content diverges while structure is preserved.** After replay, the target database has the same tables, same row counts (approximately), same query patterns, and same performance profile. But individual row values for timestamps, UUIDs, and some sequence-assigned IDs will differ.

For most use cases -- performance validation, migration testing, capacity planning -- this level of fidelity is excellent. The workload is 90%+ realistic. For byte-identical data requirements (compliance auditing, data verification), use logical replication or pg_dump instead.

## Transform for Different Targets

If you want to replay against a different system, a different dataset, or at a different scale, use the transform feature to reshape your captured workload:

```bash
# Analyze the workload structure
pg-retest transform analyze --workload workload.wkl --json

# Generate a transform plan via AI
pg-retest transform plan \
  --workload workload.wkl \
  --prompt "Scale to 10x volume, anonymize PII" \
  --provider claude \
  --output transform.toml

# Apply the plan to create a new workload
pg-retest transform apply \
  --workload workload.wkl \
  --plan transform.toml \
  --output scaled-workload.wkl
```

Transform takes your captured workload and creates a new synthetic benchmark based on the same patterns but with different data, volume, or characteristics. This is useful when:

- **Testing against a different database engine** -- capture from MySQL, transform SQL syntax, replay on PostgreSQL
- **Testing with scaled data volumes** -- 10x, 100x the captured query volume for capacity planning
- **Anonymizing production patterns** -- strip or replace identifiable data for safe sharing across teams
- **Creating repeatable benchmarks** -- turn a one-time production capture into a reusable synthetic workload

See [transform.md](transform.md) for the full transform documentation.

## Quick Reference Checklist

```
Prerequisites:
  [ ] WAL archiving enabled on source (wal_level=replica, archive_mode=on)
  [ ] Backup infrastructure supports PITR (pgBackRest, Barman, RDS snapshots, etc.)
  [ ] pg_create_restore_point privilege granted to capture user

Capture:
  [ ] Proxy deployed to 1-2 LB endpoints (or persistent proxy running)
  [ ] Applications connecting through proxy
  [ ] Start capture (restore point auto-created)
  [ ] Note restore point timestamp from log output
  [ ] Capture for desired duration (60s to hours)
  [ ] Stop capture -- .wkl file written

Restore:
  [ ] PITR restore to capture-start timestamp
  [ ] Verify restored database is accessible
  [ ] Take backup of restored copy for reuse

Replay and compare:
  [ ] pg-retest replay --workload capture.wkl --target restored-db --id-mode full --output results.wkl
  [ ] pg-retest compare --source capture.wkl --replay results.wkl --fail-on-regression

Iterate:
  [ ] Restore backup → tweak config/schema → replay → compare → repeat
```
