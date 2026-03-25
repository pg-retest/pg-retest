# ID Correlation

## Problem Statement

When PostgreSQL uses sequences (serial/IDENTITY columns) to generate primary keys, replaying a captured workload against a restored database backup produces different IDs than the original execution. This causes foreign key violations, incorrect JOINs, and misleading error rates in replay results. The ID correlation feature ensures that sequence-generated values are consistent between capture and replay, enabling accurate performance comparison for write-heavy workloads.

```
Capture:  INSERT INTO orders (...) RETURNING id   →  id=42
Capture:  SELECT * FROM orders WHERE id = 42      ←  works (42 came from the INSERT)

Replay:   INSERT INTO orders (...) RETURNING id   →  id=1001 (sequence diverged)
Replay:   SELECT * FROM orders WHERE id = 42      ←  0 rows (should be 1001)
```

## ID Handling Modes

pg-retest provides four ID handling modes via the `--id-mode` flag:

### `none` (default)

No ID handling. Sequences on the target database are left as-is. This is the existing behavior and works well for read-only replays or when the target is freshly restored from the same backup point.

### `sequence`

Snapshots all user-defined sequences from the source database at capture time and restores them on the target before replay. This ensures that `nextval()` calls during replay produce the same values as the original execution, provided the replay order matches.

**When to use:** Write workloads where INSERT ordering is deterministic and you want 1:1 ID reproduction without proxy-level capture changes.

**Requirements:** A live connection to the source database at capture time (`--source-db` flag on the `proxy` subcommand). Log-based capture emits a warning and continues without a snapshot.

### `correlate`

Captures RETURNING clause values during proxy capture and builds an ID mapping table. During replay, substitutes old IDs with new ones in subsequent queries. This handles non-deterministic ordering where sequence reset alone is insufficient, and also covers UUIDs and other non-sequence-generated values.

**When to use:** Workloads captured via `pg-retest proxy` that use RETURNING clauses (or `--id-capture-implicit` for bare INSERTs). Cross-session ID references are remapped automatically via a shared global map.

**Requirements:** Proxy-captured workload. Log-based capture (`--source-log`) does not see server responses and cannot support this mode.

### `full`

Combines `sequence` reset with `correlate` substitution for maximum fidelity. Sequences are reset first, reducing the number of diverged IDs. Correlation then fixes any remaining mismatches (UUIDs, cross-session sequence races, etc.).

**When to use:** High-fidelity write workload replay where both sequence-generated and UUID columns need remapping. Recommended for busy systems where sequence snapshot timing gaps may exist.

**Requirements:** Both a live source DB connection at capture time (for sequence snapshot) and a proxy-captured workload (for RETURNING capture).

## Usage Examples

### Proxy Capture with Sequence Snapshot

Capture a workload through the proxy while snapshotting sequences from the source database:

```bash
pg-retest proxy \
  --listen 0.0.0.0:5433 \
  --target localhost:5432 \
  --output workload.wkl \
  --id-mode sequence \
  --source-db "host=localhost port=5432 dbname=myapp user=myuser password=mypass"
```

The `--source-db` flag provides a connection string used to query `pg_sequences` before capture begins. The snapshot is embedded in the `.wkl` profile file.

### Replay with Sequence Restore

Replay the workload, restoring sequences on the target before execution:

```bash
pg-retest replay \
  --workload workload.wkl \
  --target "host=target-host dbname=myapp user=myuser password=mypass" \
  --output results.wkl \
  --id-mode sequence
```

Before replay begins, pg-retest connects to the target database and calls `setval()` for each sequence in the snapshot, resetting them to their captured state.

### Proxy Capture with RETURNING Correlation

Capture a workload with RETURNING value capture enabled:

```bash
pg-retest proxy \
  --listen 0.0.0.0:5433 \
  --target localhost:5432 \
  --output workload.wkl \
  --id-mode correlate
```

When the proxy sees a query with a RETURNING clause, it intercepts the server's DataRow responses and stores the returned values in the workload profile (up to 100 rows, 20 columns per query).

### Replay with ID Substitution

```bash
pg-retest replay \
  --workload workload.wkl \
  --target "host=target-host dbname=myapp user=myuser password=mypass" \
  --id-mode correlate
```

During replay, each query is processed in three steps:

1. **Substitute:** Known captured IDs in the SQL are replaced with the corresponding replay-time values using the global shared map.
2. **Execute:** The substituted SQL runs against the target.
3. **Register:** If the query had captured RETURNING values, the replay-time values are compared to the captured values. Differing pairs are added to the map for use by subsequent queries.

### Proxy Capture with Implicit ID Capture

For applications that do not use RETURNING clauses but rely on `currval()` or `lastval()` to retrieve generated IDs, use `--id-capture-implicit`:

```bash
pg-retest proxy \
  --listen 0.0.0.0:5433 \
  --target localhost:5432 \
  --output workload.wkl \
  --id-mode correlate \
  --id-capture-implicit \
  --source-db "host=localhost port=5432 dbname=myapp user=myuser password=mypass"
```

When this flag is enabled:

- **Auto-inject RETURNING:** If an INSERT lacks a RETURNING clause and the proxy knows the table's primary key columns (discovered from the source database at startup), the proxy appends `RETURNING <pk_columns>` to the query sent to the server and stored in the profile. The generated ID is captured as a response value.
- **currval/lastval interception:** Responses to `SELECT currval('seq_name')` and `SELECT lastval()` are captured as implicit RETURNING values and fed into the ID map.
- **Stealth mode (default):** When auto-injecting RETURNING, the proxy suppresses the extra RowDescription and DataRow messages before forwarding to the client. The client never sees the RETURNING results — it receives the same response as a bare INSERT (just CommandComplete + ReadyForQuery). This prevents driver compatibility issues with JDBC, asyncpg, Entity Framework, and other clients that don't expect result rows from a plain INSERT. Use `--no-stealth` to forward auto-injected RETURNING results to the client.

The `--source-db` flag is required when using `--id-capture-implicit` so that primary key metadata can be queried at startup.

### Full Mode (Maximum Fidelity)

```bash
# Capture
pg-retest proxy \
  --listen 0.0.0.0:5433 \
  --target localhost:5432 \
  --output workload.wkl \
  --id-mode full \
  --source-db "host=localhost port=5432 dbname=myapp user=myuser password=mypass"

# Replay
pg-retest replay \
  --workload workload.wkl \
  --target "host=target-host dbname=myapp user=myuser password=mypass" \
  --id-mode full
```

`full` mode performs sequence reset first, then enables correlation for any remaining mismatches.

### Log-Based Capture

For log-based capture (`--source-type pg-csv`), there is no live database connection available. A warning is emitted if `--id-mode sequence` is used:

```bash
pg-retest capture \
  --source-log /var/log/postgresql/postgresql.csv \
  --output workload.wkl \
  --id-mode sequence
# WARNING: Sequence snapshot will not be included. Use proxy capture with --source-db.
```

`--id-mode correlate` and `--id-mode full` are not supported for log-based capture and will emit an error.

## RETURNING Capture Internals

The proxy relay operates at the PostgreSQL wire protocol level. When `--id-mode correlate` or `--id-mode full` is active, a `CorrelateState` struct is shared between the client-to-server and server-to-client relay tasks for each connection:

- **RETURNING detection:** When the c2s relay sees a Query (`'Q'`) or Parse (`'P'`) message, it checks the SQL for a RETURNING clause (case-insensitive, not within a string literal). It pushes `true` or `false` onto a `returning_queue` (a `VecDeque<bool>`). The queue rather than a single bool supports pipelined queries in the extended query protocol.
- **RowDescription parsing:** When the s2c relay sees a `'T'` message and the front of `returning_queue` is `true`, it parses the column names and stores them (capped at 20 columns).
- **DataRow collection:** When the s2c relay sees `'D'` messages and column names are pending, it accumulates the data rows (capped at 100 rows).
- **Emit on CommandComplete:** When `'C'` arrives, the front of `returning_queue` is popped. If rows are pending, a `QueryReturning` event is emitted with the session ID, column names, and row data. The capture collector attaches this to the most recently completed query for that session.

When `--id-mode` is `none` or `sequence`, the `CorrelateState` is `None` and the `'T'` and `'D'` message arms are skipped entirely — zero overhead for non-correlation users.

## SQL Substitution State Machine

The ID substitution state machine (in `src/correlate/substitute.rs`) is a single-pass, character-level parser. It reuses the architectural pattern from `src/capture/masking.rs`.

### How Substitution Works

The machine tracks states: `Normal`, `InStringLiteral`, `InIdentifier`, `InLineComment`, `InBlockComment`, and `InNumericLiteral`. For each numeric or string literal encountered, it checks the global `IdMap`. If the literal is found in the map, it is replaced with the corresponding replay value.

**Context-aware eligibility:** The machine tracks the preceding SQL keyword to avoid substituting values in positions where a literal is not an ID:

- **Eligible positions** (substitute if in map): `WHERE`, `AND`, `OR`, `ON`, `IN`, `VALUES`, `SET`, `=`, `<>`, `!=`, `<`, `>`, `BETWEEN`, `HAVING`
- **Ineligible positions** (never substitute): `LIMIT`, `OFFSET`, `FETCH`
- **Identifier positions** (skip next token): `AS`, `FROM`, `JOIN`, `INTO`, `TABLE`, `INDEX`
- **Default (neutral):** When no keyword has set context, literals are eligible. This is the conservative choice — it may produce rare false positives but won't miss real IDs in unusual positions.

Eligibility resets after consuming exactly one literal, preventing state bleed across subqueries and multi-statement queries.

### Edge Case Handling

| SQL Fragment | Behavior |
|---|---|
| `LIMIT 42` | Not substituted (LIMIT context) |
| `WHERE id = 420` with map `{42→1001}` | Not substituted (standalone check: `420` ≠ `42`) |
| `WHERE name = 'item42'` | Not substituted (`item42` ≠ `42` as a full string value) |
| `SELECT col42 FROM t` | Not substituted (identifier context) |
| `$$contains 42$$` | Not substituted (dollar-quoted string, skipped) |
| `WHERE name = 'it''s 42'` | Not substituted (42 inside escaped string literal) |
| `WHERE balance = -42` | `-42` → minus emitted separately; `42` looked up in map |
| `WHERE price = 42.5` | `42.5` accumulated and looked up as `42.5` |
| `WHERE uuid = '550e8400-...'` | Full string content looked up; substituted if mapped |
| `WHERE id = 42 AND x IN (SELECT s FROM t LIMIT 5) AND y = 99` | `42` and `99` substituted; `5` not (LIMIT ineligibility resets after consuming `5`) |

## Cross-Session Correlation

All replay sessions share a single `IdMap` backed by `Arc<DashMap<String, String>>`. The map is lock-free for reads.

When Session A executes an INSERT and registers `42 → 1001`, Session B's subsequent query `WHERE id = 42` is automatically rewritten to `WHERE id = 1001` — even though Session B does not know about Session A's INSERT. This works because replay preserves the original timing offsets between sessions.

**Registration filtering:** Only value pairs where captured ≠ replayed are registered. If RETURNING returns `(42, 'Acme')` during capture and `(1001, 'Acme')` during replay, only `42 → 1001` is registered (`'Acme'` is the same and not added to the map).

**Column type filtering:** By default, only integer-typed and UUID-typed RETURNING columns are registered. This prevents timestamps and other frequently-changing non-ID values from polluting the map. Use `--id-correlate-all-columns` to opt into registering all differing RETURNING values including timestamps and strings.

**Timing edge case:** At very tight timing windows (<1ms between sessions), a race is possible where Session B executes before Session A's registration completes. This is the same tradeoff Oracle RAT makes and is acceptable. Mitigation: use `--speed 0.9` to add a small timing buffer if cross-session races are observed.

## Divergence Reporting

When substitution is attempted but an expected ID is not found in the map, the comparison report tracks this separately from real errors:

- `QueryResult::id_substitution_count` — how many substitutions were made for a given query
- Comparison report field `id_mismatches` — queries where expected IDs were not in the map
- Terminal report line: `ID substitutions: 1,247 | ID mismatches: 3`

This lets users distinguish real performance regressions from residual ID correlation issues.

## Replay Is Simulation, Not Replication

ID correlation significantly improves replay fidelity for write workloads, but pg-retest replay is fundamentally a **simulation**, not a replication system. Even with `--id-mode=full`:

- **Concurrent session ordering is approximate.** Replay preserves timing offsets but OS scheduling, network jitter, and connection pool behavior mean the exact interleaving of concurrent sessions will differ. Two sessions racing for the same sequence value may get different assignments than in production.
- **Non-deterministic functions produce different values.** `now()`, `random()`, `clock_timestamp()`, `gen_random_uuid()` are called at replay time, not production time. ID correlation captures and remaps the values that appear in RETURNING clauses, but values that go directly into the data without RETURNING are replayed as-is.
- **Data content diverges while structure is preserved.** After replay, the target database will have the same tables, same number of rows (approximately), same query patterns, and same performance profile as the source. But individual row values (especially timestamps, UUIDs, and sequence-assigned IDs) will differ in specific assignments.

For most use cases (performance validation, migration testing, capacity planning), this level of fidelity is excellent. For use cases requiring byte-identical data (compliance auditing, data verification), use logical replication or pg_dump instead.

## Expected Error Rates

Measured from real benchmarks (20 concurrent threads, e-commerce schema with 10 tables, SERIAL/UUID/IDENTITY columns, cross-session FK references, 100K queries):

| ID Mode | Error Rate | What Fails | What Succeeds |
|---------|-----------|------------|---------------|
| `none` | 8-15% | Duplicate keys, FK violations from sequence drift | Reads, updates to existing data |
| `sequence` | 5-10% | Cross-session FK violations (sequence ordering race) | Single-session writes, reads |
| `correlate` | 5-8% | Cross-session FK violations (timing race on ID map) | Single-session writes, RETURNING-based chains |
| `full` | 4-7% | Cross-session concurrent sequence races | Everything within a single session |

**What causes the remaining errors with `--id-mode=full`:**

The irreducible error rate comes from **cross-session concurrent sequence ordering**. When 20 sessions call `nextval('customers_id_seq')` concurrently:
- During capture: Session A gets 5867, Session B gets 5868
- During replay: Session B might get 5867, Session A gets 5868 (swapped)
- Session C references customer 5867 (meaning "Session A's customer") but gets Session B's row instead
- If Session C's reference was captured as a literal `WHERE customer_id = 5867`, the ID map may remap it correctly — or may not, depending on whether Session A or B registered their mapping first

**Drift by table type (benchmark results):**

| Table Pattern | Drift | Why |
|--------------|-------|-----|
| Single-session inserts (customers, audit_log, notifications) | **0%** | Each session manages its own IDs |
| Cross-session FK chains (orders → order_items) | **1-3%** | FK references use IDs from other sessions |
| Cascading FKs (order_items depends on orders) | **3-5%** | Failed parent → failed children |
| UUID tables (tracking_events) | **~8%** | UUIDs are random, cross-session refs common |

**How to minimize errors:**
1. Use `--id-mode=full` (sequence reset + correlation combined)
2. Use `--id-capture-implicit` (captures currval/lastval responses)
3. Reduce concurrency: `--max-connections 10` reduces sequence races
4. Use the deterministic compile step: `pg-retest workload compile` pre-resolves all IDs (eliminates runtime races, see design spec)
5. For performance testing (not correctness testing), the 4-7% error rate is acceptable — you're measuring latency distributions, not data integrity

**The 4-7% error rate is inherent to concurrent workload replay.** Oracle RAT has the same fundamental limitation and documents it as "replay divergence." The only way to achieve 0% is single-session replay or the deterministic workload compile step.

## Known Limitations

### Read-Only Replay and Data State Divergence

When using `--read-only` mode, all INSERT, UPDATE, and DELETE statements are stripped. The target database's data state does not evolve as it did during capture:

- **Missing rows:** INSERTs never execute, so subsequent SELECTs that matched those rows return fewer results. Query plans may differ.
- **Stale data:** UPDATEs never execute, so filters on updated columns see pre-update values.
- **Phantom rows:** DELETEs never execute, so rows that should be gone are still present.

Latency comparisons in read-only mode are valid for the query execution engine but not for application-level correctness.

### Sequence Mode Limitations

- **Parallel session ordering:** When multiple sessions call `nextval()` concurrently, the per-session assignment of values may differ between capture and replay. The absolute values are in the correct range, but Session A might get `id=42` during capture and `id=43` during replay. FK relationships remain valid, but the data behind each ID may differ.

- **`GENERATED ALWAYS AS IDENTITY` columns:** These use sequences internally. Sequence reset handles them, but `GENERATED ALWAYS` prevents explicit value insertion. If the sequence has drifted due to failed transactions or rollbacks on the target, values will diverge. Use `--id-mode=full` for maximum fidelity.

- **`GENERATED BY DEFAULT AS IDENTITY` columns:** Sequence reset helps the default case. Explicit IDs provided in the captured SQL are replayed as-is.

- **Sequences owned by inaccessible schemas:** `pg_sequences` only shows sequences the connected user can see. Use a superuser or a role with SELECT on all sequences.

- **Sequence cache (`CACHE` clause):** Sequences with `CACHE > 1` preallocate blocks of values per session. Even with identical starting values, different session connection ordering can cause different blocks to be assigned. For deterministic replay, set `CACHE 1` on sequences before replay (or rely on correlation mode to fix residual drift).

- **Busy system timing gap:** On a busy production system, there is a race between the sequence snapshot query and the start of capture. Other sessions may advance sequences between the snapshot and the first captured query. Proxy capture minimizes this gap (the snapshot is taken just before the proxy begins accepting connections). For log-based capture, the gap is inherent. Use `--id-mode=full` so sequence reset handles the common case and correlation catches residual drift.

- **Log-based capture cannot snapshot sequences.** Only proxy-based capture supports sequence snapshots.

- **Persistent proxy mode does not yet support sequence snapshots.** The snapshot is only taken for non-persistent (single-run) proxy mode.

- **Sequences in `pg_catalog` and `information_schema` are excluded.** Only user-defined sequences are captured.

### Implicit Capture Risks (`--id-capture-implicit`)

**`--id-capture-implicit` modifies queries sent to the database.** When enabled, the proxy appends `RETURNING <pk_columns>` to bare INSERT statements. The PostgreSQL server sends back extra RowDescription and DataRow messages that the client application did not request.

**Stealth mode (enabled by default) mitigates this risk.** The proxy captures the RETURNING data for the ID map but strips the RowDescription and DataRow messages before forwarding to the client. The client sees only CommandComplete + ReadyForQuery — exactly the same response as a bare INSERT without RETURNING. This makes `--id-capture-implicit` safe for all drivers and ORMs listed below.

**If stealth mode is disabled** (`--no-stealth`), the raw RETURNING results are forwarded to the client, which may cause compatibility issues with some drivers. The risk table below applies only when `--no-stealth` is used.

#### Driver/ORM Compatibility

| Driver/ORM | Simple Query Mode | Extended Query Mode | Risk Level | Notes |
|---|---|---|---|---|
| **psql** (CLI) | Safe | N/A | None | Displays extra result set rows; no breakage |
| **libpq** (C library) | Safe | Caution | Low | `PQexec()` handles extra result sets via `PQgetResult()` loop. `PQexecParams()` may not expect DataRows from a non-SELECT. |
| **psycopg2** (Python) | Safe | Safe | None | Handles unexpected result sets gracefully in both modes |
| **psycopg3** (Python) | Safe | Caution | Low | Pipeline mode may be affected if it doesn't expect DataRows from INSERT |
| **asyncpg** (Python) | N/A | Caution | Medium | Uses extended query protocol exclusively. May raise `unexpected message type` if it doesn't expect DataRows from a prepared INSERT. Test before use. |
| **tokio-postgres** (Rust) | Safe | Safe | None | `simple_query()` returns `Vec<SimpleQueryMessage>` which handles mixed Row/CommandComplete. `query()` (extended) expects rows. |
| **node-postgres (pg)** (Node.js) | Safe | Safe | Low | Returns `rows` array from all queries; extra RETURNING rows appear in `result.rows` |
| **JDBC** (Java) | Caution | Caution | Medium | `Statement.executeUpdate()` returns an int (row count), not a ResultSet. Auto-injected RETURNING produces a ResultSet where an int was expected. May throw `PSQLException: A result was returned when none was expected.` |
| **Hibernate/JPA** (Java) | N/A | Caution | Medium | Uses JDBC underneath. Batch inserts may break if the driver doesn't handle unexpected result sets. |
| **Go pgx** | Safe | Caution | Low | `Exec()` discards result rows. `Query()` / `QueryRow()` handle them. Extended protocol with `Prepare` may not expect DataRows. |
| **Go database/sql + lib/pq** | Caution | Caution | Medium | `Exec()` calls may break if driver tries to parse unexpected DataRows as an error |
| **Rails ActiveRecord** (Ruby) | Safe | Safe | Low | Uses `pg` gem which handles extra result sets |
| **Django** (Python) | Safe | Safe | None | Uses psycopg2/psycopg3 underneath |
| **SQLAlchemy** (Python) | Safe | Safe | None | Uses psycopg2/psycopg3 underneath |
| **Entity Framework** (C#/Npgsql) | N/A | Caution | Medium | Npgsql uses extended query protocol. `ExecuteNonQuery()` may not expect DataRows from a bare INSERT with auto-injected RETURNING. |

**General rule:** If your application uses `simple_query` mode (sending raw SQL strings), auto-inject is generally safe. If your application uses the extended query protocol with prepared statements and explicitly typed parameters, test first.

**Recommendation:** For applications using JDBC, asyncpg, Go lib/pq, or Entity Framework, prefer `--id-mode=correlate` WITHOUT `--id-capture-implicit` and ensure your application uses explicit `RETURNING` clauses. Alternatively, use `--id-mode=sequence` which does not modify any queries.

### Correlate Mode Limitations

- **Proxy-captured workloads only.** Log-based capture (`--source-log`) does not see server responses. `--id-mode=correlate` on a log-captured workload emits an error.

- **Requires RETURNING clauses (or `--id-capture-implicit`).** By default, only queries with explicit `RETURNING` clauses have their response values captured. Applications that do not use RETURNING need `--id-capture-implicit` enabled.

- **Cross-session timing dependency.** The global shared map relies on replay timing offsets to ensure Session A's INSERT completes and registers before Session B's SELECT. At very tight timing windows (<1ms), a race is possible. Use `--speed 0.9` to add a timing buffer if needed.

- **Substitution false positives.** The state machine is not a full SQL parser. Edge cases where false positives may occur:
  - A numeric literal that matches a captured ID inside a function call (e.g., `SUBSTR(col, 42, 10)`)
  - String values that match captured UUIDs inside LIKE patterns or concatenations
  - Deeply nested subqueries where keyword eligibility tracking may lose context

  False positives are rare in practice. The substituted value is still a valid value of the same type. The divergence report tracks all substitutions for auditing.

- **Compound/multi-statement queries.** Semicolon-separated statements in a single `simple_query` call are treated as a flat token stream. A LIMIT in the first statement could theoretically affect eligibility tracking for the second statement. In practice this is uncommon because most applications send one statement per query call.

- **Row and column caps.** Only the first 100 rows and 20 columns of each RETURNING result are captured. Queries returning more are partially captured with a warning.

### UUID and Non-Sequence ID Limitations

- **UUIDs are not sequence-resettable.** `--id-mode=sequence` has no effect on UUID columns. Only `--id-mode=correlate` or `--id-mode=full` can handle UUIDs.

- **Application-generated IDs.** IDs generated by the application (snowflake IDs, ULID, hash-based IDs, timestamp-based IDs) are embedded in the captured SQL as literals. pg-retest replays them as-is. If the application generates the same ID deterministically (e.g., hash of input data), replay works. If not, the captured IDs may conflict with existing data on the target.

- **Computed/generated columns (`GENERATED ALWAYS AS (expr) STORED`).** These are computed from other columns and cannot be inserted directly. The database computes them automatically during replay. If a generated column is used in a subsequent WHERE clause and the computation depends on a remapped value, the generated result will also differ — this second-order effect is not tracked.

- **`DEFAULT` expressions that are not sequences.** Columns with `DEFAULT now()`, `DEFAULT current_timestamp`, `DEFAULT random()`, or other non-deterministic defaults produce different values during replay. These are only tracked if the query uses RETURNING to capture them.

### Scaled Replay and ID Correlation

When using `--scale N` with `--id-mode=correlate` or `--id-mode=full`:
- Each scaled copy of a session gets its own ID remapping (different IDs from the database, different map entries).
- Cross-session correlation still works (all sessions share one global map).
- The global map will be larger (N times as many entries), but `DashMap` scales well under concurrent access.
- Sequence reset is less effective because N copies calling `nextval()` exhaust the sequence range N times faster. `--id-mode=full` is recommended for scaled write workloads.

### What Is Not Handled (Out of Scope)

- **DB-link / cross-database references:** IDs that flow between databases via foreign data wrappers or application logic.
- **Triggers that generate IDs:** BEFORE INSERT triggers show the final value in RETURNING (correct). AFTER INSERT triggers that insert into other tables are not captured.
- **COPY command:** Bulk loading via COPY does not support RETURNING. IDs generated during COPY are not capturable. Use `--id-mode=sequence` for best-effort coverage.
- **Logical replication slots:** Sequence state is not replicated by logical replication. If the target was provisioned via logical replication instead of PITR restore, sequences may be at completely different positions.
- **TLS for sequence snapshot in proxy mode.** The proxy `--source-db` connection currently uses NoTls. If TLS is required, include `sslmode=require` in the connection string (handled by the PostgreSQL driver).
