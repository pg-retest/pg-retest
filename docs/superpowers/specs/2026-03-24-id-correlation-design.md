# ID Correlation Design Spec

**Date:** 2026-03-24
**Branch:** `feature/id-correlation`
**Status:** Draft

---

## Problem Statement

When replaying captured database workloads against a target, database-generated IDs (sequences, UUIDs, identity columns) differ from the source. Subsequent queries referencing those IDs fail silently -- SELECTs return zero rows, UPDATEs affect nothing, FK inserts violate constraints. The replay error rate becomes meaningless noise, masking real performance differences.

```
Capture:  INSERT INTO orders (...) RETURNING id   →  id=42
Capture:  SELECT * FROM orders WHERE id = 42      ←  works (42 came from the INSERT)

Replay:   INSERT INTO orders (...) RETURNING id   →  id=1001 (sequence diverged)
Replay:   SELECT * FROM orders WHERE id = 42      ←  0 rows (should be 1001)
```

No open-source PostgreSQL tool solves this. Oracle RAT does it at the kernel level for $11,500/processor.

---

## Solution: Tiered ID Modes

New `--id-mode` flag on both `capture` and `replay` subcommands:

| Mode | Behavior | Requires |
|------|----------|----------|
| `none` | Current behavior, no ID handling (default) | Nothing |
| `sequence` | Snapshot sequences at capture, reset at replay | Live source DB connection at capture time |
| `correlate` | Capture RETURNING values via proxy, remap during replay | Proxy-captured workload |
| `full` | Sequence reset + correlation combined | Both of the above |

**Implementation phases:**
- **Phase 1:** `none` + `sequence` mode
- **Phase 2:** `correlate` + `full` mode

---

## Architecture

### New Module: `src/correlate/`

```
src/correlate/
    mod.rs          -- IdMode enum, public API re-exports
    sequence.rs     -- SequenceState, snapshot_sequences(), restore_sequences()
    capture.rs      -- RETURNING detection, DataRow collection, ResponseRow types
    map.rs          -- IdMap (Arc<DashMap> wrapper), register(), lookup()
    substitute.rs   -- SQL rewriting state machine, rewrite_sql()
```

### Data Types

```rust
// correlate/mod.rs
#[derive(Debug, Clone, PartialEq, Eq, clap::ValueEnum)]
pub enum IdMode {
    None,
    Sequence,
    Correlate,
    Full,
}

// correlate/sequence.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceState {
    pub schema: String,
    pub name: String,
    pub last_value: Option<i64>,  // None if nextval never called
    pub increment_by: i64,
    pub start_value: i64,
    pub min_value: i64,
    pub max_value: i64,
    pub cycle: bool,
    pub is_called: bool,
}

// correlate/capture.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseRow {
    pub columns: Vec<(String, String)>,  // (column_name, text_value)
}

// correlate/map.rs
pub struct IdMap {
    inner: Arc<DashMap<String, String>>,  // captured_value → replay_value
}
```

### Profile Format Changes

Backward-compatible additions to existing structs:

```rust
// profile/mod.rs — Query struct
#[serde(default)]
pub response_values: Option<Vec<ResponseRow>>,

// profile/mod.rs — Metadata struct
#[serde(default)]
pub sequence_snapshot: Option<Vec<SequenceState>>,
```

Both `Option` + `#[serde(default)]` so existing v2 `.wkl` files load cleanly.

---

## Phase 1: Sequence Mode

### Capture-Time Sequence Snapshot

When `--id-mode=sequence` or `full`, query the source database before capture begins:

```sql
-- pg_sequences (PG 10+) has all needed columns in one view
SELECT
    schemaname,
    sequencename,
    last_value,
    increment_by,
    start_value,
    min_value,
    max_value,
    cycle,
    last_value IS NOT NULL AS is_called
FROM pg_sequences
WHERE schemaname NOT IN ('pg_catalog', 'information_schema');
```

Store the snapshot in `profile.metadata.sequence_snapshot`.

**Graceful degradation:** If capture is log-based and no live DB connection is available, emit a warning and continue without snapshot. Do not fail hard.

```
WARN: --id-mode=sequence requires a live connection to the source database.
      Sequence snapshot skipped — ID mode will have no effect during replay.
```

### Replay-Time Sequence Reset

Before spawning any replay session tasks, reset all sequences on the target:

```sql
-- If is_called is true:
SELECT setval('schema.sequence_name', last_value, true);

-- If is_called is false (nextval never called):
SELECT setval('schema.sequence_name', start_value, false);
```

Per-sequence error handling -- missing sequences emit a warning, do not abort:

```
WARN: Sequence "public.orders_id_seq" not found on target — skipping.
INFO: Sequence sync complete — 14 sequences reset, 0 skipped, 0 errors.
```

### Sequence Snapshot Timing on Busy Systems

On a busy production system, there is a race between the sequence snapshot query and the start of capture. Between the moment we read `last_value` and the moment the first captured query executes, other (non-captured) sessions may advance the sequence. This means the snapshot may be slightly behind the actual state when capture begins.

**Mitigation strategies:**

1. **Proxy capture is self-consistent.** When using the proxy, capture starts when the first client connects through the proxy. Sequences are snapshotted just before the proxy begins accepting connections. Since only traffic through the proxy is captured, and no proxy traffic flows during the snapshot, the snapshot is consistent with the start of captured traffic.

2. **Log-based capture has an inherent gap.** The log contains queries from ALL connections, including those not related to the capture window. The snapshot is a best-effort approximation. For maximum accuracy, users should snapshot on a quiet system or accept that `--id-mode=full` (sequence + correlation) will catch any residual drift via the correlate layer.

3. **Advisory lock during snapshot (optional).** For users who need deterministic sequence state, the snapshot could acquire a brief `ACCESS EXCLUSIVE` lock on each sequence during the read. This is NOT the default (too disruptive) but could be exposed as `--sequence-lock` for users who accept the brief write pause.

The recommended workflow for busy systems: use `--id-mode=full` so sequence reset handles the common case and correlation catches the edge cases.

### Concurrency Caveat

Sequence reset works perfectly for single-session replay. For parallel replay, concurrent `nextval()` calls may produce values in a different order than capture. This is acceptable and documented. Absolute values per-transaction are correct when transactions are isolated.

---

## Phase 2: Correlate Mode

### Proxy Capture Flow

**Shared correlation state:** Created in `handle_connection_inner` and `Arc`-shared into both relay tasks:

```rust
struct CorrelateState {
    /// Queue of RETURNING expectations (not a bool flag — supports pipelined queries)
    returning_queue: Mutex<VecDeque<bool>>,
    /// Column names from most recent RowDescription
    pending_columns: Mutex<Vec<String>>,
    /// Accumulated DataRows for current RETURNING query
    pending_rows: Mutex<Vec<Vec<Option<String>>>>,
}
```

`returning_queue` is a `VecDeque<bool>` (not a single bool) because the extended query protocol allows pipelining: a client can send multiple Parse/Bind/Execute sequences before reading results. Each query push `true/false` onto the queue; the s2c relay pops from the front when it sees a CommandComplete to know whether the preceding DataRows were from a RETURNING query.

**RETURNING detection:** When `relay_client_to_server` sees a Query (`'Q'`) or Parse (`'P'`) message, check if the SQL contains a RETURNING clause. Push `true/false` onto `returning_queue`.

**RowDescription parsing:** New `protocol.rs` function:
```rust
pub fn extract_row_description(msg: &PgMessage) -> Option<Vec<String>>
```
When s2c relay sees `'T'` and `returning_queue` front is `true`, parse and store column names in `pending_columns`. Cap at 20 columns (skip capture if exceeded, emit warning).

**DataRow collection:** New `protocol.rs` function:
```rust
pub fn extract_data_row(msg: &PgMessage, num_columns: usize) -> Option<Vec<Option<String>>>
```
When s2c relay sees `'D'` and `pending_columns` is non-empty, accumulate rows in `pending_rows`. Cap at 100 rows (stop accumulating after 100, emit warning if more arrive).

**Emit on CommandComplete:** When `'C'` arrives, pop from `returning_queue`. If `pending_rows` is non-empty, emit:
```rust
CaptureEvent::QueryReturning {
    session_id: u64,
    columns: Vec<String>,
    rows: Vec<Vec<Option<String>>>,
    timestamp: Instant,
}
```
Then clear `pending_columns` and `pending_rows`.

**Collector handling:** Both `run_collector` and `run_staging_collector` in `capture.rs` must handle `QueryReturning` events. The event is associated with the most recent pending query for that session. `response_values` is populated on the `CapturedQuery` (or `StagingRow` via a new JSON-encoded TEXT column).

**Capture scope:** Only queries with RETURNING have their responses captured. All other queries (plain SELECTs, UPDATEs without RETURNING) are not captured. No heuristics about column types or names.

**Performance:** When `id_mode` is `None` or `Sequence`, the `CorrelateState` is `None` and the `'T'` and `'D'` arms remain in the `_ => {}` catch-all. Zero overhead for non-correlation users.

### Implicit ID Capture (`--id-capture-implicit`)

Optional flag, off by default. Only effective with `--id-mode=correlate` or `--id-mode=full`.

When enabled, handles the common case where applications do NOT use RETURNING:

```sql
-- Pattern 1: bare INSERT (ORM default)
INSERT INTO orders (customer, total) VALUES ('Acme', 99.99);
-- Application calls currval() or lastval() to get the generated ID

-- Pattern 2: explicit currval/lastval
SELECT currval('orders_id_seq');
SELECT lastval();
```

**Two mechanisms:**

1. **Auto-inject RETURNING:** During proxy capture, if an INSERT lacks a RETURNING clause and targets a table with a known PK, the proxy appends `RETURNING <pk_columns>` to BOTH the query sent to the server AND the SQL stored in the profile. The server returns the generated ID values, which are captured as `response_values` in the profile. At replay time, the RETURNING version executes and the replay engine compares captured vs replayed values to build the ID map. (The server must see the RETURNING clause too -- otherwise there are no response DataRows to capture.)

2. **currval/lastval interception:** `SELECT currval(...)` and `SELECT lastval()` responses are captured as implicit RETURNING values and fed into the IdMap.

**PK discovery at capture start:** When `--id-capture-implicit` is enabled, query for table PK columns at capture start (same connection as sequence snapshot). Store as `pk_map` in profile metadata.

```sql
SELECT
    kcu.table_schema,
    kcu.table_name,
    kcu.column_name,
    kcu.ordinal_position
FROM information_schema.table_constraints tc
JOIN information_schema.key_column_usage kcu
    USING (constraint_schema, constraint_name, table_schema, table_name)
WHERE tc.constraint_type = 'PRIMARY KEY'
    AND tc.table_schema NOT IN ('pg_catalog', 'information_schema')
ORDER BY kcu.table_schema, kcu.table_name, kcu.ordinal_position;
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TablePk {
    pub schema: String,
    pub table: String,
    pub columns: Vec<String>,  // PK column names in order
}
```

Profile metadata addition (backward-compatible):
```rust
#[serde(default)]
pub pk_map: Option<Vec<TablePk>>,
```

**CLI:**
```
--id-capture-implicit
    Auto-inject RETURNING for bare INSERTs and intercept currval/lastval
    responses during proxy capture. Only effective with --id-mode=correlate
    or --id-mode=full. Default: off.
```

**Error on log-captured workload:**
```
ERROR: --id-mode=correlate requires proxy-captured workload.
       Re-capture using `pg-retest proxy` or use --id-mode=sequence instead.
```

### Global Shared ID Map

All replay sessions share a single `IdMap` backed by `Arc<DashMap<String, String>>`.

```rust
impl IdMap {
    pub fn new() -> Self;
    pub fn register(&self, captured: String, replayed: String);
    pub fn substitute<'a>(&self, sql: &'a str) -> Cow<'a, str>;  // avoids allocation when no subs
    pub fn len(&self) -> usize;
}
```

**Registration filtering -- only register values that actually differ:**

When comparing captured vs replayed RETURNING values, only register `(captured, replayed)` pairs where `captured != replayed`. This is the primary defense against map pollution:

- If `RETURNING id, created_at, name` returns `(42, '2024-03-08', 'Acme')` during capture and `(1001, '2026-03-24', 'Acme')` during replay:
  - `42 → 1001` is registered (different)
  - `'2024-03-08' → '2026-03-24'` is registered (different -- timestamps diverge)
  - `'Acme'` is NOT registered (same value)

**Timestamp collision mitigation:** Timestamps that differ between capture and replay will enter the map. This is acceptable -- a `WHERE created_at = '2024-03-08'` in a subsequent query *should* be rewritten if that timestamp came from a prior RETURNING clause. However, to reduce false matches on common timestamp patterns, only register values from integer-typed and UUID-typed columns by default. Expose `--id-correlate-all-columns` flag to opt into registering all differing RETURNING values including timestamps and strings.

**Cross-session correlation:** Session A inserts a row and registers `42 → 1001`. Session B's subsequent query `WHERE id = 42` is rewritten to `WHERE id = 1001`. This works because replay preserves timing offsets -- Session A's INSERT executes before Session B's SELECT, just as it did during capture. The DashMap provides lock-free concurrent reads.

**Edge case:** At very tight timing windows (<1ms between sessions), a race is possible where Session B executes before Session A's registration completes. This is the same tradeoff Oracle RAT makes with SCN synchronization and is acceptable.

### Replay-Time Flow

For each query in a session:

```rust
// Step 1: Substitute known IDs in the SQL
let effective_sql = match &id_map {
    Some(map) => map.substitute(&query.sql),
    None => query.sql.clone(),
};

// Step 2: Execute
let result = client.simple_query(&effective_sql).await;

// Step 3: If query had captured RETURNING values, register mappings
if let (Ok(ref rows), Some(ref map), Some(ref captured)) =
    (&result, &id_map, &query.response_values)
{
    // Match by column position (not name)
    // For each (captured_val, replay_val) where they differ:
    //   map.register(captured_val, replay_val)
}
```

Column matching is positional. If column count doesn't match (schema changed), skip registration and emit a warning.

### `full` Mode

Combines both phases sequentially:
1. Reset sequences (Phase 1 logic)
2. Enable correlation (Phase 2 logic)

Sequence reset reduces the number of diverged IDs. Correlation fixes the ones that still diverge (UUIDs, cross-session sequence races, etc.).

---

## SQL Substitution State Machine

Reuses the architectural pattern from `capture/masking.rs`.

### States

```
Normal           -- outside any literal or identifier
InStringLiteral  -- inside '...' (accumulate content, check map at end)
InIdentifier     -- inside "..." (skip)
InLineComment    -- inside -- ... (skip)
InBlockComment   -- inside /* ... */ (skip)
InNumericLiteral -- accumulating digits (candidate for substitution)
```

### Algorithm

1. Walk SQL character by character, tracking state
2. In `Normal`, encountering a digit → enter `InNumericLiteral`, accumulate
3. When numeric literal ends, check:
   - Standalone: not preceded/followed by alphanumeric or underscore
   - Look up in DashMap. If found → emit replacement. If not → emit original.
4. In `InStringLiteral`, accumulate content. On closing quote, look up full content (without quotes) in map. If found → emit replacement in quotes.
5. Dollar-quoted strings (`$$...$$`) treated as string literals, content skipped.

### Value Position Filtering

Lightweight keyword tracking in `Normal` state with **single-literal scope**. Each keyword sets the eligibility for exactly the *next* literal encountered, then resets to neutral. This prevents state bleed across subqueries and parenthesized expressions.

- **Eligible (next literal):** `WHERE`, `AND`, `OR`, `ON`, `IN`, `VALUES`, `SET`, `=`, `<>`, `!=`, `<`, `>`, `BETWEEN`, `HAVING` → next literal eligible for substitution, then reset to neutral
- **Ineligible (next literal):** `LIMIT`, `OFFSET`, `FETCH` → next literal skipped, then reset to neutral
- **Identifier context (next token):** `AS`, `FROM`, `JOIN`, `INTO`, `TABLE`, `INDEX` → next token skipped, then reset to neutral
- **Parentheses and commas:** do NOT change eligibility state. A `(` after `IN` preserves the eligible state for each literal in the IN-list. Commas between VALUES entries preserve eligibility.
- **Default (neutral):** when no keyword has set eligibility, literals ARE eligible for substitution. This is the conservative choice -- it may produce rare false positives but won't miss real IDs in unusual SQL positions.

### Performance

Single pass, O(tokens) per query. DashMap `.get()` is O(1). <1us for typical queries (100-500 chars).

### Edge Cases

| Case | Behavior |
|------|----------|
| `LIMIT 42` | Not substituted (LIMIT context) |
| `WHERE id = 420` (map has `42→1001`) | Not substituted (standalone check) |
| `WHERE name = 'item42'` | Not substituted (inside string, `item42` ≠ `42`) |
| `SELECT col42 FROM t` | Not substituted (identifier context) |
| `$$contains 42$$` | Not substituted (dollar-quoted string) |
| `WHERE name = 'it''s 42'` | Not substituted (42 inside escaped string) |
| `WHERE balance = -42` | `-42` → minus separate, `42` looked up |
| `WHERE price = 42.5` | Accumulated as `42.5`, looked up as `42.5` |
| `WHERE uuid = '550e8400-...'` | String content looked up, substituted if mapped |
| `WHERE id = 42 AND x IN (SELECT s FROM t LIMIT 5) AND y = 99` | 42 and 99 substituted, 5 not (LIMIT ineligibility resets after consuming `5`) |

---

## Divergence Reporting

When substitution fails (an ID literal is found in a query but not in the map), report it separately from real errors:

- New field in `QueryResult`: `id_substitution_count: usize` (how many substitutions were made)
- New field in comparison report: `id_mismatches: usize` (queries where expected IDs were not in the map)
- Terminal report shows: `ID substitutions: 1,247 | ID mismatches: 3`

This lets users distinguish real regressions from residual ID issues.

---

## Testing

### Unit Tests (in `src/correlate/` module tests)

**`sequence.rs`:**
- `test_sequence_state_roundtrip` -- serialize/deserialize all field combinations
- `test_sequence_state_null_last_value` -- is_called=false, last_value=None
- `test_snapshot_query_construction` -- verify generated SQL is schema-qualified
- `test_restore_setval_called` -- setval with is_called=true
- `test_restore_setval_not_called` -- setval with is_called=false

**`capture.rs`:**
- `test_detect_returning_simple` -- `INSERT INTO t (x) RETURNING id`
- `test_detect_returning_multiple_columns` -- `RETURNING id, uuid, created_at`
- `test_detect_returning_case_insensitive` -- `returning`, `RETURNING`, `Returning`
- `test_no_returning` -- plain INSERT, SELECT, UPDATE
- `test_returning_in_string_literal` -- `VALUES ('use RETURNING')` is not RETURNING
- `test_row_cap_at_100` -- 200 DataRows → only first 100 stored
- `test_auto_inject_returning` -- bare INSERT + known PK → RETURNING appended in profile
- `test_auto_inject_no_pk` -- bare INSERT to unknown table → no injection
- `test_auto_inject_composite_pk` -- table with multi-column PK → all PK columns in RETURNING
- `test_detect_currval` -- `SELECT currval('seq')` detected as implicit RETURNING
- `test_detect_lastval` -- `SELECT lastval()` detected as implicit RETURNING
- `test_no_implicit_without_flag` -- implicit capture disabled by default

**`map.rs`:**
- `test_register_and_lookup` -- basic insert/get
- `test_concurrent_register` -- 10 tasks, 100 entries each, all present
- `test_substitute_delegates` -- verify substitute() calls state machine
- `test_map_len` -- count after registrations

**`substitute.rs`:**
- `test_integer_in_where` -- `WHERE id = 42` → `WHERE id = 1001`
- `test_integer_in_and` -- `WHERE a = 1 AND b = 42` → both substituted
- `test_integer_in_values` -- `VALUES (42, 'foo')` → `VALUES (1001, 'foo')`
- `test_integer_in_set` -- `SET order_id = 42` → `SET order_id = 1001`
- `test_integer_in_join_on` -- `ON a.id = 42` → substituted
- `test_integer_in_in_list` -- `WHERE id IN (42, 43, 44)` → all substituted
- `test_no_substitute_in_limit` -- `LIMIT 42` unchanged
- `test_no_substitute_in_offset` -- `OFFSET 42` unchanged
- `test_no_substitute_in_string` -- `WHERE name = 'item42'` unchanged
- `test_no_substitute_partial_match` -- `WHERE id = 420` with map `{42→1001}` unchanged
- `test_no_substitute_in_identifier` -- `SELECT col42 FROM t` unchanged
- `test_no_substitute_in_table_name` -- `FROM table42` unchanged
- `test_uuid_in_where` -- `WHERE uuid = '550e8400-...'` → substituted
- `test_uuid_no_partial` -- UUID substring not matched
- `test_dollar_quoted_string` -- `$$contains 42$$` unchanged
- `test_escaped_quotes` -- `'it''s 42'` unchanged
- `test_negative_number` -- `-42` mapped correctly
- `test_decimal_number` -- `42.5` looked up as `42.5`
- `test_multiple_substitutions` -- 3 different mapped values in one query
- `test_no_map_entries` -- empty map, SQL unchanged
- `test_between_clause` -- `BETWEEN 42 AND 100`
- `test_compound_query` -- semicolon-separated statements
- `test_subquery_with_limit` -- `WHERE id = 42 AND status IN (SELECT s FROM t LIMIT 5) AND x = 99` -- 42 and 99 substituted, 5 not
- `test_cte_with_limit` -- `WITH cte AS (SELECT * FROM t LIMIT 10) SELECT * FROM cte WHERE id = 42` -- 42 substituted, 10 not
- `test_eligibility_resets_after_literal` -- LIMIT ineligibility does not persist past one literal

### Integration Tests (in `tests/`)

**Profile format:**
- `test_sequence_snapshot_in_profile` -- write/read profile with SequenceState
- `test_profile_backward_compat` -- load old `.wkl` without new fields
- `test_response_values_in_profile` -- write/read profile with ResponseRow
- `test_id_mode_none_unchanged` -- replay with none produces identical behavior

**Per-ID-type (each requires PG):**
- `test_correlate_serial` -- SERIAL column, INSERT RETURNING, SELECT by id
- `test_correlate_identity` -- GENERATED ALWAYS AS IDENTITY, same flow
- `test_correlate_uuid_v4` -- DEFAULT gen_random_uuid(), INSERT RETURNING, SELECT by uuid
- `test_correlate_uuid_extension` -- DEFAULT uuid_generate_v4(), same flow
- `test_correlate_sequence_nextval` -- explicit named sequence via nextval()
- `test_correlate_composite_key` -- (tenant_id, GENERATED id) composite PK
- `test_correlate_multiple_returning` -- RETURNING id, created_at, uuid
- `test_correlate_insert_select_update_delete` -- full lifecycle through remapped ID
- `test_correlate_fk_chain` -- INSERT orders RETURNING id → INSERT order_items (order_id)

**Sequence mode per-ID-type:**
- `test_sequence_mode_serial` -- sequence reset only, SERIAL
- `test_sequence_mode_identity` -- sequence reset only, IDENTITY

**Full mode:**
- `test_full_mode_serial` -- sequence reset + correlation, SERIAL
- `test_full_mode_uuid` -- sequence reset irrelevant, correlation handles UUID

**Cross-session:**
- `test_cross_session_correlation` -- Session A inserts, Session B selects by that ID

**Extended query protocol:**
- `test_correlate_prepared_statement_returning` -- prepared INSERT with RETURNING via Parse/Bind/Execute
- `test_correlate_pipelined_queries` -- multiple Parse/Bind/Execute pipelined, RETURNING queue tracks correctly

**Implicit capture (each requires PG):**
- `test_implicit_bare_insert_serial` -- INSERT without RETURNING on SERIAL column, auto-injected
- `test_implicit_bare_insert_uuid` -- INSERT without RETURNING on UUID column, auto-injected
- `test_implicit_currval_workflow` -- INSERT → SELECT currval() → SELECT by id, all remapped
- `test_implicit_lastval_workflow` -- INSERT → SELECT lastval() → UPDATE by id, all remapped
- `test_implicit_disabled_by_default` -- same workflow without flag, no capture, queries fail on replay
- `test_implicit_fk_chain_no_returning` -- bare INSERT parent → bare INSERT child with currval(), FK satisfied

**Error handling:**
- `test_missing_sequence_on_target` -- warning emitted, replay continues
- `test_correlate_requires_proxy` -- error on log-captured workload
- `test_sequence_no_source_connection` -- warning, no snapshot, replay proceeds

### Benchmarks (criterion, in `benches/`)

- `bench_substitute_no_map` -- empty map baseline
- `bench_substitute_small_map` -- 10 entries, ~200 char query
- `bench_substitute_large_map` -- 10,000 entries, ~200 char query (prove O(tokens) not O(map_size))
- `bench_substitute_complex_query` -- 2000-char query with subqueries, CTEs
- `bench_substitute_uuid_heavy` -- 5 UUID comparisons, 1000 UUIDs in map
- `bench_data_row_parsing` -- DataRow messages (1, 5, 20 columns)
- `bench_row_description_parsing` -- RowDescription messages
- `bench_returning_detection` -- RETURNING detection across 1000 mixed queries

---

## CLI Interface

### Capture subcommand
```
pg-retest capture --source-log <path> --id-mode <mode> [--source-host ...] -o workload.wkl
pg-retest proxy --listen 0.0.0.0:5433 --target <connstring> --id-mode <mode> -o workload.wkl
```

### Replay subcommand
```
pg-retest replay workload.wkl --target <connstring> --id-mode <mode>
```

### Help text
```
--id-mode <MODE>  [default: none]
    none       No ID handling (current behavior)
    sequence   Snapshot sequence state at capture, reset on target before replay.
               Eliminates ID divergence for serial/bigserial/identity columns.
               Parallel sessions may see different ordering of sequence values.
    correlate  Capture RETURNING values via proxy, substitute during replay.
               Requires proxy-captured workload. Handles sequences, UUIDs, and
               any value returned by RETURNING clauses. Cross-session correlation
               via shared map (timing-dependent).
    full       Sequence reset + correlation combined. Maximum ID fidelity.
               Sequence reset reduces divergence; correlation fixes remainder.
```

---

## Documentation

- README: new "ID Correlation" section under Replay Modes
- README: "Known Limitations" additions for each mode
- `--help` text on both `capture` and `replay` subcommands
- Rustdoc on all public types/functions in `correlate/`
- `docs/id-correlation.md`: detailed guide with capture→replay→substitution examples

---

## Known Limitations & Caveats

This section documents fundamental limitations that users MUST understand. These are not bugs -- they are inherent tradeoffs in workload replay. Each limitation should be documented in the README, `--help` output, and `docs/id-correlation.md`.

### Read-Only Replay and Data State Divergence

When using `--read-only` mode, all INSERT, UPDATE, and DELETE statements are stripped from the workload. This means the target database's data state does NOT evolve the way it did during capture:

- **Missing rows:** INSERTs that created rows during capture never execute. Subsequent SELECTs that matched those rows will return fewer results on the target. Query plans may differ (different row estimates → different join strategies).
- **Stale data:** UPDATEs that changed column values during capture never execute. SELECTs that filter on updated columns will see pre-update values. Aggregations (SUM, AVG, COUNT) will return different results.
- **Phantom rows:** DELETEs that removed rows during capture never execute. SELECTs will return rows that should have been deleted, inflating result sets.
- **Index divergence:** Without writes, index statistics don't change. The target's `pg_statistics` may differ from what the source looked like mid-workload, leading to different plan choices.

**Impact on comparison:** Latency comparisons in read-only mode are valid for the *query execution engine* (same query, same plan, different config). They are NOT valid for *application-level correctness* (same query, same result set). Users should understand they are testing "how fast does this query run" not "does this query return the right data."

### Sequence Mode Limitations

**Parallel session ordering:** When multiple sessions call `nextval()` concurrently, the order of calls may differ between capture and replay. Session A might get `id=42` during capture but `id=43` during replay (and Session B gets the opposite). The absolute values are from the correct range, but the per-session assignment may differ. This means:
- Queries like `SELECT * FROM orders WHERE id = 42` may return Session B's row instead of Session A's row
- FK relationships are still valid (the IDs exist), but the data behind each ID may belong to a different logical session

**GENERATED ALWAYS AS IDENTITY columns:** These use sequences internally, so `--id-mode=sequence` handles them. However, `GENERATED ALWAYS` prevents explicit value insertion -- you cannot override the generated value. Sequence reset ensures the next generated value matches, but if the sequence has drifted due to failed transactions or rollbacks on the target, the values will diverge. Use `--id-mode=full` for maximum fidelity.

**GENERATED BY DEFAULT AS IDENTITY columns:** These allow explicit value insertion but default to a sequence. If the captured workload sometimes provides explicit IDs and sometimes relies on the default, sequence reset only helps the default case. Explicit IDs in the captured SQL are replayed as-is.

**Sequences owned by other schemas:** `pg_sequences` shows all sequences the connected user can see. If sequences are in schemas the capture user can't access, they won't be snapshotted. Use a superuser or a role with SELECT on all sequences.

**Sequence cache (`CACHE` clause):** PostgreSQL sequences with `CACHE > 1` preallocate blocks of values per session. Even with identical starting values, different session connection ordering can cause different blocks to be assigned. Sequence reset sets `last_value` but does not reset per-session caches. For deterministic replay, set `CACHE 1` on sequences before replay (or accept the divergence and rely on correlation mode).

**Busy system timing gap:** See the "Sequence Snapshot Timing on Busy Systems" section above. On a busy production system, there is a race between the snapshot and the first captured query.

### Correlate Mode Limitations

**Proxy-captured workloads only:** Correlation requires RETURNING value capture, which only works when the workload is captured via the proxy (`pg-retest proxy`). Log-based capture (`--source-log`) does not see server responses. `--id-mode=correlate` on a log-captured workload emits an error.

**Requires RETURNING clauses (or `--id-capture-implicit`):** By default, only queries with explicit `RETURNING` clauses have their response values captured. Applications that don't use RETURNING (bare INSERTs followed by `currval()` or `lastval()`) need `--id-capture-implicit` enabled to get coverage.

**Cross-session timing dependency:** The global shared map relies on replay timing offsets to ensure Session A's INSERT completes and registers the ID before Session B's SELECT tries to use it. At very tight timing windows (<1ms between sessions), a race is possible. This is the same tradeoff Oracle RAT makes. Mitigation: use `--speed 0.9` to add a small timing buffer if cross-session races are observed.

**Substitution false positives:** The state machine performs context-aware substitution but is not a full SQL parser. Edge cases where false positives may occur:
- A numeric literal that happens to match a captured ID in an unusual SQL position (e.g., inside a function call like `SUBSTR(col, 42, 10)`)
- String values that match captured UUIDs inside LIKE patterns or concatenations
- Deeply nested subqueries where keyword eligibility tracking may lose context

False positives are rare in practice and the substituted value is still a valid value of the same type. The divergence report tracks all substitutions so users can audit.

**Compound / multi-statement queries:** Semicolon-separated statements in a single `simple_query` call are treated as a flat token stream. A LIMIT in the first statement could theoretically affect eligibility tracking for the second statement. In practice this is uncommon because most applications send one statement per query call.

### UUID and Non-Sequence ID Limitations

**UUIDs are not sequence-resettable:** `--id-mode=sequence` has no effect on UUID columns. UUIDs generated by `gen_random_uuid()` or `uuid_generate_v4()` are random by definition. Only `--id-mode=correlate` or `--id-mode=full` can handle UUIDs, by capturing the generated value via RETURNING and remapping during replay.

**Application-generated IDs:** IDs generated by the application (snowflake IDs, ULID, hash-based IDs, timestamp-based IDs) are embedded in the captured SQL as literals. pg-retest has no way to know these are "IDs" versus any other value. They are replayed as-is. If the application generates the same ID deterministically (e.g., hash of input data), replay works. If the application generates IDs from timestamps or randomness, replay will insert with the captured (old) IDs, which may conflict with existing data on the target.

**Computed / generated columns (`GENERATED ALWAYS AS (expr) STORED`):** These are NOT the same as identity columns. Generated columns are computed from other columns in the same row. They cannot be inserted or updated directly. pg-retest does not need to handle them specially -- the database computes them automatically during replay. However, if a generated column is used in a subsequent WHERE clause and the computation depends on a column whose value was remapped, the generated value will also differ. This is a second-order effect that is not tracked.

**DEFAULT expressions that are not sequences:** Columns with `DEFAULT now()`, `DEFAULT current_timestamp`, `DEFAULT random()`, or other non-deterministic defaults will produce different values during replay. If these values are referenced in subsequent queries, they will not be in the ID map unless the query used RETURNING to capture them.

### Scaled Replay and ID Correlation

When using `--scale N` with `--id-mode=correlate` or `--id-mode=full`:
- Each scaled copy of a session gets its own ID remapping (different IDs from the database, different map entries)
- Cross-session correlation still works (all sessions share one global map)
- The global map will be larger (N times as many entries), but DashMap scales well
- Sequence reset is less effective because N copies calling `nextval()` will exhaust the sequence range N times faster. `--id-mode=full` is recommended for scaled write workloads.

### What Is NOT Handled (Out of Scope)

- **DB-link / cross-database references:** IDs that flow between databases via foreign data wrappers or application logic
- **Triggers that generate IDs:** BEFORE INSERT triggers that set column values are invisible to RETURNING (they show the final value, which is correct) but AFTER INSERT triggers that insert into other tables are not captured
- **COPY command:** Bulk loading via COPY does not support RETURNING. IDs generated during COPY are not capturable. Use `--id-mode=sequence` for best-effort coverage.
- **Logical replication slots:** Sequence state is not replicated by logical replication. If the target was provisioned via logical replication instead of PITR restore, sequences may be at completely different positions.

---

## What NOT To Do

- Do not change default behavior -- `--id-mode=none` is the default
- Do not attempt app-generated ID heuristics (snowflakes, hash-based, timestamp-based)
- Do not store full result sets -- cap at 100 rows / 20 columns
- Do not break existing `.wkl` backward compatibility
- Do not make Phase 1 depend on proxy capture
- Do not use regex for substitution -- use the character-level state machine

---

## Dependencies

- `dashmap` crate for `IdMap` (lock-free concurrent HashMap)
- `criterion` crate for benchmarks (may already be present)
- No other new dependencies

---

## Implementation Order

### Phase 1 (PR 1)
1. Add `IdMode` enum and `--id-mode` CLI flag on both subcommands
2. Add `SequenceState` struct and `sequence_snapshot` field to profile metadata
3. Implement `snapshot_sequences()` with graceful degradation
4. Implement `restore_sequences()` with per-sequence error handling
5. Phase 1 unit tests + integration tests
6. Update docs and help text for sequence mode

### Phase 2 (PR 2)
1. Add `ResponseRow` and `response_values` field to Query struct
2. Add `extract_row_description()` and `extract_data_row()` to `protocol.rs`
3. Add RETURNING detection + DataRow collection to proxy relay
4. Add `CaptureEvent::QueryReturning` variant
5. Implement `IdMap` with `DashMap`
6. Implement substitution state machine
7. Wire into replay loop (substitute → execute → register)
8. Add `full` mode (sequence reset + correlation)
9. Add `--id-capture-implicit` flag
10. Implement auto-inject RETURNING (PK discovery + INSERT rewriting in profile)
11. Implement currval/lastval interception
12. Add `TablePk` struct and `pk_map` to profile metadata
13. Phase 2 unit tests + integration tests + implicit capture tests + benchmarks
14. Update docs for correlate, full, and implicit capture modes
