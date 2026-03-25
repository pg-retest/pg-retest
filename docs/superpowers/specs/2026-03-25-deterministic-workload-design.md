# Deterministic Workload Compilation Design Spec

**Date:** 2026-03-25
**Branch:** `feature/id-correlation`
**Status:** Draft
**Depends on:** [ID Correlation Design](2026-03-24-id-correlation-design.md) (Phase 2)

---

## Problem Statement

The `--id-mode=correlate` and `--id-mode=full` modes solve the ID divergence problem during replay, but they introduce two costs:

1. **Runtime overhead.** Every query passes through the substitution state machine and every RETURNING result triggers a DashMap insert. For large workloads (100k+ queries), the cumulative cost is measurable.

2. **Cross-session timing race.** The global shared `IdMap` relies on replay timing to ensure Session A's INSERT registers an ID before Session B's SELECT needs it. At tight timing windows (<1ms), the race is real and produces intermittent failures that are difficult to debug.

Both costs stem from the same root cause: ID resolution happens at replay time, when timing is non-deterministic. If we could resolve all IDs *before* replay, both problems disappear.

---

## Solution Overview

Introduce a **compile** step that transforms a proxy-captured workload (with `response_values`) into a self-contained workload where all ID references are pre-resolved in the SQL text.

```
                    compile
raw.wkl ──────────────────────────> deterministic.wkl
(has response_values,               (no response_values,
 original SQL with                   SQL contains resolved
 captured IDs)                       target-compatible IDs)
```

The compiled `.wkl` file replays with zero ID handling -- no `--id-mode` flag, no IdMap, no substitution engine. It is a plain workload file where the SQL already contains the correct IDs for the target database state at that PITR point.

### Workflow

```bash
# Step 1: Capture with full ID tracking
pg-retest proxy --listen 0.0.0.0:5433 --target source-db:5432 \
    --id-mode=full -o raw.wkl

# Step 2: Compile the workload (offline, no DB connection needed)
pg-retest workload compile raw.wkl -o deterministic.wkl

# Step 3: Replay with no special flags
pg-retest replay deterministic.wkl --target backup-db:5432
```

---

## CLI Interface

### New Subcommand

```
pg-retest workload compile <input.wkl> -o <output.wkl> [OPTIONS]

Arguments:
    <input.wkl>    Source workload file (must have response_values from
                   --id-mode=correlate or --id-mode=full capture)

Options:
    -o, --output <path>     Output file path (required)
    --dry-run               Show substitution stats without writing output
    --verbose               Print each substitution as it is applied
    --strict                Fail if any query references an ID not found
                            in the captured response_values
```

### Validation

The compile command validates at startup:

1. Input file must be a valid v2+ `.wkl` with `response_values` present on at least one query.
2. If no queries have `response_values`, emit an error:
   ```
   ERROR: No response_values found in raw.wkl.
          Capture with --id-mode=correlate or --id-mode=full to enable compilation.
   ```
3. If `sequence_snapshot` is present, include it in the output unchanged (useful if the user later wants to combine with sequence reset on a different target).

---

## Algorithm

The compile step walks all sessions and queries in chronological order, building an ID map incrementally and substituting as it goes. This mirrors what the replay engine would do at runtime, but deterministically and offline.

### Step-by-Step

```
1. Load input WorkloadProfile
2. Initialize empty IdMap (HashMap<String, String>, not DashMap -- single-threaded)
3. Sort all queries across all sessions by timestamp (global order)
4. For each query in global chronological order:
   a. Run the substitution state machine on query.sql using the current IdMap
      - This rewrites any captured IDs to their "replayed" equivalents
      - Store the rewritten SQL back into query.sql
   b. If query.response_values is Some:
      - For each ResponseRow in response_values:
        - For each (column_name, captured_value) pair:
          - The captured_value IS the "replayed" value in a compile context
            (because at capture time, this was the real DB response)
          - Register: captured_value -> captured_value (identity mapping)
      - Wait -- this needs clarification. See "Mapping Logic" below.
   c. Clear query.response_values (set to None)
5. Update profile metadata:
   - Set capture_method to indicate compiled status (append "+compiled")
   - Preserve sequence_snapshot if present
6. Write output .wkl
```

### Mapping Logic

The key insight is understanding what `response_values` contains and what we want to substitute.

During **proxy capture**, a query like `INSERT INTO orders (...) RETURNING id` produces:
- `query.sql` = `INSERT INTO orders (customer_id, total) VALUES (5, 99.99) RETURNING id`
- `query.response_values` = `[ResponseRow { columns: [("id", "42")] }]`

The value `42` is the **source database's** generated ID. Subsequent queries in the captured workload reference `42`:
- `SELECT * FROM orders WHERE id = 42`
- `INSERT INTO order_items (order_id, ...) VALUES (42, ...)`

During **runtime replay** with `--id-mode=correlate`:
- The INSERT runs on the target and returns `id = 1001`
- IdMap registers: `42 -> 1001`
- Subsequent queries get `42` replaced with `1001`

During **compilation**, there is no target database. We cannot know that `42` will become `1001`. Instead, the compile step does something different: **it bakes the captured IDs into the SQL so that the workload is self-consistent with the captured response values.**

This means:
- The captured `response_values` already contain the IDs that subsequent queries reference
- The SQL already contains those same IDs
- **No substitution is needed for the common case** where capture and replay use the same PITR restore

But wait -- the whole point is that the target database (restored from PITR) will generate *different* IDs because sequences may have diverged. The compile step cannot fix this without knowing the target.

### Revised Model: Compile as Sequence-Pinned Snapshot

The compile step works correctly under one critical assumption: **the target database is restored to the exact PITR point where capture occurred, AND sequences are reset** (which `--id-mode=full` handles via `sequence_snapshot`).

With sequences reset to their capture-time state, the target database will generate the **same IDs** as the source did during capture. Therefore:

1. The captured `response_values` contain the IDs the target will also generate
2. Subsequent queries already reference those IDs in their SQL
3. The compile step's job is to **validate** this consistency and **strip `response_values`** (since they are no longer needed)

For the case where IDs might still differ (UUIDs, application-generated IDs), the compile step cannot help -- those are inherently non-deterministic. The compile step handles the **sequence-based ID** case where PITR + sequence reset guarantees determinism.

### Revised Algorithm

```
1. Load input WorkloadProfile
2. Validate: sequence_snapshot must be present (warn if absent)
3. Build a set of all captured response values (the "known IDs"):
   - Walk all queries, collect every value from response_values
   - Store as known_ids: HashSet<String>
4. Walk all queries in chronological order:
   a. Scan query.sql for literals that appear in known_ids
      - Use the substitution state machine in "audit mode" (detect but don't replace)
      - Count how many known IDs appear in this query's SQL
   b. Record the count for the compilation report
   c. Clear query.response_values (set to None)
5. Update metadata:
   - Set id_mode to None (no runtime ID handling needed)
   - Add compiled_from: Some("full") to indicate provenance
   - Preserve sequence_snapshot (critical for replay)
6. Write output .wkl

Report:
  Queries with response_values: 1,247
  Unique captured IDs: 1,193
  Queries referencing captured IDs: 3,891
  Total ID references in SQL: 5,422
  Compilation complete: deterministic.wkl (no --id-mode needed at replay)
```

### Alternative Model: Full Offline Substitution (Future)

A more powerful variant would accept a target connection string and:

1. Reset sequences on the target
2. Execute each INSERT with RETURNING against the target (in a transaction that gets rolled back)
3. Collect the actual target IDs
4. Build the real capture->target IdMap
5. Substitute all IDs in the workload SQL
6. Write the compiled .wkl with target-specific IDs

This is more complex but would handle UUID divergence. It is left as a future enhancement.

---

## Data Flow

```
┌──────────────────────────────────────────────────────────────────┐
│                     pg-retest workload compile                    │
│                                                                  │
│  ┌─────────┐    ┌──────────────┐    ┌────────────┐    ┌───────┐ │
│  │ Load    │───>│ Collect      │───>│ Validate   │───>│ Strip │ │
│  │ raw.wkl │    │ known IDs    │    │ references │    │ resp  │ │
│  │         │    │ from resp_   │    │ in SQL     │    │ vals  │ │
│  │         │    │ values       │    │            │    │       │ │
│  └─────────┘    └──────────────┘    └────────────┘    └───┬───┘ │
│                                                           │     │
│                                         ┌─────────────────┘     │
│                                         v                       │
│                                    ┌─────────┐                  │
│                                    │ Write   │                  │
│                                    │ det.wkl │                  │
│                                    └─────────┘                  │
└──────────────────────────────────────────────────────────────────┘
```

### Input Requirements

| Field | Required | Purpose |
|-------|----------|---------|
| `response_values` on queries | Yes (at least 1) | Source of captured IDs |
| `sequence_snapshot` in metadata | Recommended | Ensures target generates matching IDs |
| `capture_method` | Any | Preserved with "+compiled" suffix |

### Output Characteristics

| Field | Value | Why |
|-------|-------|-----|
| `response_values` | `None` on all queries | No longer needed |
| `sequence_snapshot` | Preserved from input | Still needed for replay-time sequence reset |
| `capture_method` | Original + "+compiled" | Provenance tracking |
| `id_mode` metadata | `None` | Replay needs no ID handling |
| SQL text | Unchanged | IDs already match (PITR + seq reset) |

---

## Limitations

### Requires PITR + Sequence Reset

The compiled workload assumes the target database is an exact PITR restore of the source at capture time, with sequences reset. If the target's sequences are at different positions, the generated IDs will not match the SQL references.

**Mitigation:** The compiled `.wkl` retains `sequence_snapshot`, so `pg-retest replay` can still reset sequences even without `--id-mode`. This should be automatic when `sequence_snapshot` is present.

### Cannot Handle UUID Divergence

UUIDs generated by `gen_random_uuid()` are random. Even with identical database state, the target will generate different UUIDs. The compile step cannot predict target UUIDs without a live connection.

**Mitigation:** For UUID-heavy workloads, use runtime `--id-mode=full` instead of compilation. Or use the future "online compile" variant that executes against the target.

### Incompatible with `--scale`

Scaled replay duplicates sessions. Each duplicate would need its own set of IDs, which are unknown at compile time. The compile step produces a file suitable for 1:1 replay only.

**Mitigation:** If scaling is needed, use runtime `--id-mode=full`.

### Larger File Size (Marginal)

In practice, the file size difference is negligible because:
- SQL text is unchanged (IDs are already in the SQL from capture)
- `response_values` are removed (reduces file size)
- Net effect is typically a **smaller** file

### Single PITR Point

The compiled file is specific to one database state. If you restore to a different PITR point, the sequence values and data state will differ, and the baked-in IDs may not match.

### No Partial Compilation

Either all `response_values` are processed or none. There is no mode to compile some sessions and leave others for runtime correlation.

---

## Implementation Notes

### Reuses Existing Machinery

The compile step reuses these existing components:

1. **`profile::load()` / `profile::save()`** -- MessagePack I/O for `.wkl` files
2. **`correlate::substitute::SubstitutionStateMachine`** -- Used in "audit mode" to scan for ID references without modifying SQL
3. **`correlate::capture::ResponseRow`** -- Existing struct for captured RETURNING values

### New Code Required

1. **`src/workload/compile.rs`** (~150 lines)
   - `compile_workload(input: &WorkloadProfile) -> CompileResult`
   - `CompileResult { profile: WorkloadProfile, stats: CompileStats }`
   - `CompileStats { queries_with_response: usize, unique_ids: usize, referencing_queries: usize, total_references: usize }`

2. **CLI addition in `src/cli.rs`**
   - New `Workload` subcommand group with `Compile` variant
   - Or add `compile` as a top-level subcommand (simpler, matches `inspect`)

3. **Integration in `src/main.rs`**
   - Wire the compile subcommand to load, compile, and save

### Module Placement

```
src/workload/
    mod.rs          -- pub mod compile
    compile.rs      -- compile_workload(), CompileResult, CompileStats
```

Or, if keeping it simpler:

```
src/correlate/
    compile.rs      -- compile_workload() alongside existing correlate code
```

The second option is preferred since compilation is conceptually part of the ID correlation feature.

### Testing

**Unit tests (in `correlate/compile.rs`):**

- `test_compile_strips_response_values` -- all response_values become None
- `test_compile_preserves_sequence_snapshot` -- snapshot passes through
- `test_compile_preserves_sql` -- SQL text is unchanged
- `test_compile_updates_capture_method` -- suffix "+compiled" added
- `test_compile_stats_accurate` -- counts match expected values
- `test_compile_no_response_values_error` -- workload without response_values rejected
- `test_compile_empty_workload` -- zero sessions handled gracefully
- `test_compile_audit_finds_references` -- known IDs detected in subsequent SQL

**Integration tests:**

- `test_compile_roundtrip` -- capture with full mode, compile, replay without id-mode, verify no errors
- `test_compile_replay_matches_full` -- compare results of compiled replay vs full-mode replay (should be identical)
- `test_compile_with_sequence_reset` -- compiled file triggers sequence reset on replay

### Backward Compatibility

- No changes to the `.wkl` format
- No changes to existing subcommands
- The `workload compile` subcommand is purely additive
- Compiled `.wkl` files are valid v2 format (just with `response_values: None` everywhere)

---

## Open Questions

1. **Should `compile` be a subcommand of `workload` or top-level?** Leaning toward `workload compile` since it operates on workload files, similar to how `git stash` is a subgroup. But `pg-retest compile` is shorter and more discoverable.

2. **Should the compiled file auto-trigger sequence reset?** If `sequence_snapshot` is present, replay could unconditionally reset sequences. This is useful but changes default behavior for users who manually add sequence_snapshot without wanting reset. A metadata flag like `auto_sequence_reset: true` could make this explicit.

3. **Should `--strict` abort on first missing reference or collect all?** Collecting all and reporting at the end is more useful for debugging. Abort-on-first is faster for CI. Default to collect-all, `--fail-fast` for abort-on-first.

4. **Online compile variant priority.** The future "online compile" (with target DB connection) would make this feature dramatically more useful by handling UUIDs. Should it be in scope for v1 or deferred?

---

## Why This Is Powerful

| Property | Runtime Correlation | Deterministic Compile |
|----------|--------------------|-----------------------|
| Runtime overhead | Substitution per query + DashMap ops | Zero |
| Cross-session race | Possible at <1ms windows | Impossible (pre-resolved) |
| Determinism | Timing-dependent | Same file = same replay, always |
| Portability | Needs `--id-mode` flag + understanding | Just a `.wkl` file |
| UUID support | Yes | No (sequence-based IDs only) |
| Scaled replay | Yes | No |
| Target dependency | Any compatible target | Must be exact PITR + seq reset |

The deterministic compile mode is the **safest and most reproducible** way to replay a captured workload when the target is a known PITR restore. It eliminates an entire class of intermittent failures (timing races) and makes `.wkl` files truly portable artifacts.
