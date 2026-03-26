# Replay Accuracy & Fidelity

pg-retest is a workload simulation tool. This document explains exactly how accurate replay is, what causes errors, and how to minimize them.

## TL;DR

| Workload Type | Accuracy | Error Source |
|--------------|----------|-------------|
| Read-only (`--read-only`) | ~100% | None (all SELECTs execute) |
| Write, intra-session IDs, `--id-mode=full` | **99.98%** | Rare sequence race within transactions |
| Write, cross-session IDs, `--id-mode=full` | 93-96% | Cross-session execution ordering race |
| Write, concurrent, `--id-mode=none` | 85-92% | Sequence drift + ordering race |

**The accuracy depends heavily on your application's ID reference pattern:**

- **Intra-session** (each session creates and uses its own IDs): **99.98%** — near-perfect. Measured: 96 errors / 468,387 queries with 20 concurrent threads, 62% writes.
- **Cross-session** (Session B references IDs created by Session A): **93-96%** — the ordering race causes 4-7% FK failures. Measured: 6,160 errors / 99,919 queries.

Most real applications are closer to the intra-session pattern (a web request creates a resource and operates on it within the same DB connection). Connection-pooled apps with shared state are closer to cross-session.

## What "Accuracy" Means

Replay accuracy measures: **of the queries in the captured workload, what percentage execute without error on the target?**

An error during replay means a query that succeeded during capture fails during replay — typically a foreign key violation, duplicate key, or missing row. These are artifacts of the replay process, not real application bugs.

**Replay accuracy does NOT mean data identity.** Even at 100% query success, the data on the target will differ from the source in timestamps, UUID values, and sequence-assigned IDs. Replay preserves the statistical profile and performance characteristics, not byte-identical data.

## Accuracy by Mode

### Read-Only Replay (`--read-only`)

**Accuracy: ~100%**

All INSERT, UPDATE, and DELETE statements are stripped. Only SELECTs execute. Since no data is modified, there are no FK violations or duplicate keys.

**Caveats:**
- The target database's data state doesn't evolve during replay. If a captured SELECT matched a row that was INSERTed earlier in the workload, that row doesn't exist on the target (the INSERT was stripped). The SELECT still executes but may return fewer rows.
- Query plans may differ because table statistics don't change (no writes = no ANALYZE).
- Aggregations (COUNT, SUM, AVG) will return different values if the underlying data doesn't match.

**Best for:** Performance testing of query execution — "do my SELECTs run at the same speed on the new target?"

### Write Workloads by ID Mode

Measured from real benchmarks: 100K queries, 20 concurrent threads, e-commerce schema with 10 tables (SERIAL, UUID, IDENTITY columns, cross-session FK references).

#### `--id-mode=none` (default)

**Accuracy: 85-92%**

No ID handling. Sequences on the target are wherever they happen to be after PITR restore.

| Error Type | Cause | Rate |
|-----------|-------|------|
| Duplicate key | Sequence on target produces an ID that already exists | 3-5% |
| FK violation (sequence drift) | INSERT references a parent ID that got a different sequence value | 3-5% |
| FK violation (ordering) | Cross-session: child INSERT runs before parent | 2-4% |

**Use when:** Testing read-heavy workloads with some writes, or when you just want a quick performance comparison and don't care about write error rates.

#### `--id-mode=sequence`

**Accuracy: 90-95%**

Sequences are snapshotted at capture time and reset on the target before replay. Eliminates duplicate key errors from sequence drift.

| Error Type | Cause | Rate |
|-----------|-------|------|
| FK violation (ordering) | Cross-session: concurrent sessions consume sequence values in different order | 5-10% |

**Use when:** Write workloads where most sessions are independent (few cross-session FK references).

#### `--id-mode=correlate`

**Accuracy: 92-96%**

RETURNING values are captured through the proxy. During replay, a global shared IdMap remaps captured IDs to replay-generated IDs.

| Error Type | Cause | Rate |
|-----------|-------|------|
| FK violation (ordering) | Session B references Session A's ID before Session A has registered it in the map | 4-8% |

**Requires:** Proxy-captured workload (not log-based capture).

**Use when:** Write workloads with RETURNING clauses or `--id-capture-implicit` enabled.

#### `--id-mode=full` (recommended for writes)

**Accuracy: 93-96%**

Sequence reset + correlation combined. Sequences are reset (handles the common case), then correlation catches remaining divergence.

| Error Type | Cause | Rate |
|-----------|-------|------|
| FK violation (ordering) | Cross-session concurrent execution ordering race | 4-7% |

**Use when:** Maximum fidelity for write-heavy workloads.

#### `pg-retest compile` (deterministic)

**Accuracy: Same as `full` (93-96%)**

Pre-resolves IDs by stripping response_values and marking the workload as compiled. The replay engine auto-resets sequences. Does NOT improve accuracy over `full` because the root cause is execution ordering, not ID remapping.

**Use when:** CI/CD pipelines where you want deterministic behavior and don't want to pass `--id-mode` flags. Also reduces replay-time overhead (no IdMap, no substitution engine).

## What Causes the Remaining 4-7% Errors

The irreducible error rate comes from **cross-session concurrent execution ordering**. This is a fundamental limitation of concurrent workload replay, shared by all replay tools including Oracle RAT (which documents it as "replay divergence").

### The Problem

During capture, 20 threads execute concurrently:
```
t=0ms   Session A: INSERT INTO customers (...) → gets id=5867
t=2ms   Session B: INSERT INTO orders (customer_id=5867, ...) → succeeds (5867 exists)
```

During replay, the same 20 sessions execute concurrently but OS scheduling, network jitter, and connection pool behavior change the exact interleaving:
```
t=0ms   Session B: INSERT INTO orders (customer_id=5867, ...) → FK VIOLATION (5867 doesn't exist yet!)
t=1ms   Session A: INSERT INTO customers (...) → gets id=5867 → too late
```

The ID `5867` is correct. The ID map has the right mapping. The problem is **when** each session executes, not **what** ID it uses.

### Per-Table Impact

| Table Pattern | Drift | Why |
|--------------|-------|-----|
| Single-session inserts (each session creates and uses its own IDs) | **0%** | No cross-session dependency |
| Cross-session FK chains (Session A creates parent, Session B creates child) | **1-3%** | Ordering race on the FK reference |
| Cascading FKs (child depends on parent that depends on grandparent) | **3-5%** | Failed parent → all children fail |
| Tables with high cross-session reference rates | **5-8%** | More sessions referencing each other = more ordering conflicts |

### Why `compile` Doesn't Help

The compile step pre-resolves IDs in the SQL text. But the IDs are already correct — they match the source capture exactly. The compile step validates this and strips response_values. The errors come from execution ordering, which compile cannot fix (it runs offline without a target DB).

## Generated ID Types — Detailed Behavior

| ID Type | How Handled | Single-Session | Concurrent Sessions |
|---------|-------------|----------------|-------------------|
| `SERIAL` / `BIGSERIAL` | Sequence snapshot + reset | Same IDs (deterministic) | May swap values between sessions |
| `GENERATED ALWAYS AS IDENTITY` | Same as SERIAL (uses sequences) | Same IDs | Same as SERIAL |
| `GENERATED BY DEFAULT AS IDENTITY` | Sequence reset for default case; explicit IDs replayed as-is | Same IDs | Same as SERIAL |
| `UUID` (`gen_random_uuid()`) | Captured via RETURNING or `--id-capture-implicit`. Mapped in IdMap. | Remapped correctly | Cross-session refs may race |
| `currval()` / `lastval()` | Intercepted by `--id-capture-implicit`. Response captured. | Captured correctly | Cross-session use may race |
| Application-generated IDs (snowflake, ULID, hash) | **Not handled** — replayed as captured literals | Works if deterministic | Fails if timestamp/random-based |
| `DEFAULT now()` / `DEFAULT random()` | **Not handled** unless RETURNING captures them | Different values on replay | N/A |
| `COPY` bulk loads | **Not handled** — COPY doesn't support RETURNING | Use `--id-mode=sequence` | N/A |

## How to Maximize Accuracy

### For Performance Testing (recommended approach)

1. Use `--id-mode=full` — gets you to 93-96% accuracy
2. Accept the 4-7% error rate — it doesn't affect performance metrics
3. Focus on latency distributions (p50, p95, p99) and throughput, not error counts
4. The queries that succeed have realistic performance characteristics

### For Maximum Write Accuracy

1. Use `--id-mode=full --id-capture-implicit` during capture
2. Use `--max-connections N` during replay where N < capture concurrency (reduces ordering races)
3. Consider sequential replay (`--max-connections 1`) for near-100% accuracy (loses timing realism)

### For Zero-Error Replay

Use the synthetic workload generator (`demo/synthesize-workload.py`):
- Analyzes your captured workload's statistical fingerprint
- Generates a new workload with **fixed IDs** (no auto-gen, no sequences, no UUIDs)
- All cross-session references are pre-resolved
- Replay has **zero** dependency on execution ordering
- 100% query template match rate with the captured workload

```bash
python3 demo/synthesize-workload.py \
    --input captured.wkl \
    --source-db "host=localhost dbname=mydb user=demo password=demo" \
    --output-workload synthetic.wkl \
    --output-data synthetic-data.sql

# Load data, then replay
psql target-db < synthetic-data.sql
pg-retest replay synthetic.wkl --target target-db
```

### For CI/CD Pipelines

1. Capture once with `--id-mode=full`
2. Compile: `pg-retest compile workload.wkl -o workload-compiled.wkl`
3. Check `workload-compiled.wkl` into your repo
4. In CI: restore PITR → replay compiled workload → compare → pass/fail

## Comparison with Oracle RAT

Oracle Real Application Testing (RAT) is the industry benchmark for database replay. Here's how pg-retest compares:

| Aspect | Oracle RAT | pg-retest |
|--------|-----------|-----------|
| Capture method | Kernel-level | Wire protocol proxy |
| ID remapping | Sequence + ROWID at kernel level | Sequence reset + RETURNING capture + IdMap |
| Cross-session ordering | SCN-based commit ordering (3 modes) | Timing-offset based (concurrent) |
| Divergence rate | "Replay divergence" documented, not quantified | 4-7% measured and documented |
| UUID handling | SYS_GUID via KEEP privilege | Via RETURNING capture |
| Cost | ~$11,500/processor + Enterprise Edition | Free (Apache 2.0) |
| Database support | Oracle only | PostgreSQL (+ MySQL capture) |

Oracle RAT's SCN synchronization mode can reduce divergence by enforcing commit ordering, but it still documents "replay divergence" as an expected outcome. The fundamental problem — concurrent sessions may interleave differently — is the same.

## Future: Dependency-Graph Ordered Replay

The only known approach to eliminate cross-session ordering errors is **dependency-graph construction** (the DoppelGanger++ approach from VLDB 2024):

1. During capture, analyze which sessions' writes feed into other sessions' reads
2. Build a directed acyclic graph of session dependencies
3. During replay, enforce the dependency ordering: only execute Session B's query after Session A's INSERT has committed

This is a significant engineering effort and is tracked as a future enhancement. It would bring concurrent write workload accuracy to near-100% while preserving timing realism.

## References

- [Oracle Database Replay documentation](https://docs.oracle.com/en/database/oracle/oracle-database/19/ratug/replaying-a-database-workload.html)
- [DoppelGanger++ (VLDB 2024)](https://dl.acm.org/doi/10.1145/3639322) — Fast dependency graph generation for database replay
- [Consistent Synchronization Schemes for Workload Replay (VLDB 2011)](http://www.vldb.org/pvldb/vol4/p1225-morfonios.pdf) — Oracle's commit-ordering algorithms
