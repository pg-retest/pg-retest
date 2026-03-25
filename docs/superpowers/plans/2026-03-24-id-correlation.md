# ID Correlation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add database-generated ID capture and remapping to pg-retest so replayed workloads produce correct results even when sequences, UUIDs, and identity columns diverge on the target.

**Architecture:** New `src/correlate/` module with four submodules: `sequence.rs` (snapshot/restore), `capture.rs` (RETURNING detection), `map.rs` (global DashMap), `substitute.rs` (SQL rewriting state machine). Profile format extended with backward-compatible `Option` fields. Proxy extended to parse server DataRow responses. Replay engine extended with pre-query substitution and post-query ID registration.

**Tech Stack:** Rust, dashmap crate, criterion (benchmarks), tokio-postgres, rmp-serde (MessagePack profiles)

**Spec:** `docs/superpowers/specs/2026-03-24-id-correlation-design.md`

---

## File Map

### New Files (Phase 1)
| File | Responsibility |
|------|---------------|
| `src/correlate/mod.rs` | `IdMode` enum, `pub mod` declarations, re-exports |
| `src/correlate/sequence.rs` | `SequenceState` struct, `snapshot_sequences()`, `restore_sequences()` |
| `tests/id_correlation_test.rs` | Phase 1 integration tests (profile roundtrip, backward compat, sequence reset) |

### New Files (Phase 2)
| File | Responsibility |
|------|---------------|
| `src/correlate/capture.rs` | `ResponseRow`, `TablePk`, RETURNING detection, `has_returning()`, `detect_currval_lastval()`, `inject_returning()` |
| `src/correlate/map.rs` | `IdMap` wrapper around `Arc<DashMap>`, `register()`, `substitute()`, `len()` |
| `src/correlate/substitute.rs` | SQL rewriting state machine, `substitute_ids()` function |
| `tests/id_correlate_test.rs` | Phase 2 integration tests (per-ID-type, cross-session, implicit capture) |
| `benches/substitute_bench.rs` | Criterion benchmarks for substitution, DataRow parsing, RETURNING detection |

### Modified Files
| File | Changes |
|------|---------|
| `src/lib.rs` | Add `pub mod correlate;` |
| `src/cli.rs` | Add `--id-mode` to `CaptureArgs`, `ReplayArgs`, `ProxyArgs`. Add `--id-capture-implicit`, `--id-correlate-all-columns` to `ProxyArgs`/`ReplayArgs`. |
| `src/profile/mod.rs` | Add `response_values: Option<Vec<ResponseRow>>` to `Query`. Add `sequence_snapshot`, `pk_map` to `Metadata`. Import types. |
| `src/proxy/protocol.rs` | Add `extract_row_description()`, `extract_data_row()` |
| `src/proxy/connection.rs` | Add `CorrelateState` shared struct, RETURNING detection in c2s relay, DataRow/RowDescription parsing in s2c relay |
| `src/proxy/capture.rs` | Add `CaptureEvent::QueryReturning` variant, handle in both collectors, extend `CapturedQuery` |
| `src/proxy/staging.rs` | Add `response_values_json` column to `StagingRow` and SQLite schema |
| `src/replay/mod.rs` | Add `id_substitution_count` to `QueryResult`. Accept `IdMap` in `run_replay()`. |
| `src/replay/session.rs` | Add substitution before execution, registration after execution. Accept `Option<IdMap>`. |
| `src/main.rs` | Wire `--id-mode` through capture/proxy/replay entry points |
| `Cargo.toml` | Add `dashmap = "6"`, add `[dev-dependencies] criterion` with `[[bench]]` entry |

---

## Phase 1: Sequence Mode

### Task 1: Add `correlate` Module Skeleton + `IdMode` Enum

**Files:**
- Create: `src/correlate/mod.rs`
- Modify: `src/lib.rs:5` (add `pub mod correlate;` between `compare` and `config`)
- Modify: `src/cli.rs:87-123` (CaptureArgs), `src/cli.rs:125-186` (ReplayArgs), `src/cli.rs:233-286` (ProxyArgs)

- [ ] **Step 1: Create `src/correlate/mod.rs` with `IdMode` enum**

```rust
pub mod sequence;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// ID handling mode for capture and replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdMode {
    /// No ID handling (default, current behavior)
    None,
    /// Snapshot sequences at capture, reset on target before replay
    Sequence,
    /// Capture RETURNING values via proxy, substitute during replay (Phase 2)
    Correlate,
    /// Sequence reset + correlation combined (Phase 2)
    Full,
}

impl Default for IdMode {
    fn default() -> Self {
        IdMode::None
    }
}

impl IdMode {
    /// Whether this mode requires sequence snapshot/restore.
    pub fn needs_sequences(&self) -> bool {
        matches!(self, IdMode::Sequence | IdMode::Full)
    }

    /// Whether this mode requires correlation (proxy RETURNING capture).
    pub fn needs_correlation(&self) -> bool {
        matches!(self, IdMode::Correlate | IdMode::Full)
    }
}
```

- [ ] **Step 2: Create empty `src/correlate/sequence.rs`**

```rust
// Sequence snapshot and restore — implementation in Task 3.
```

- [ ] **Step 3: Register module in `src/lib.rs`**

Add `pub mod correlate;` after `pub mod compare;` (alphabetical, line 5 area).

- [ ] **Step 4: Add `--id-mode` to CLI args**

Add to `CaptureArgs` (after `mask_values` field, ~line 111):
```rust
    /// ID handling mode: none, sequence, correlate, full
    #[arg(long, value_enum, default_value_t = crate::correlate::IdMode::None)]
    pub id_mode: crate::correlate::IdMode,
```

Add identical field to `ReplayArgs` (after `tls_ca_cert`, ~line 185) and `ProxyArgs` (after `max_capture_duration`, ~line 285).

- [ ] **Step 5: Verify it compiles**

Run: `cargo build 2>&1 | tail -5`
Expected: Build succeeds with no errors.

- [ ] **Step 6: Verify `--help` shows the new flag**

Run: `cargo run -- capture --help 2>&1 | grep id-mode`
Expected: `--id-mode <ID_MODE>  ID handling mode: none, sequence, correlate, full [default: none]`

- [ ] **Step 7: Commit**

```bash
git add src/correlate/mod.rs src/correlate/sequence.rs src/lib.rs src/cli.rs
git commit -m "feat(correlate): add IdMode enum and --id-mode CLI flag"
```

---

### Task 2: Add `SequenceState` + Profile Format Extension

**Files:**
- Modify: `src/correlate/sequence.rs`
- Modify: `src/profile/mod.rs:91-96` (Metadata struct)
- Test: `tests/id_correlation_test.rs` (new file)

- [ ] **Step 1: Write the failing test — sequence snapshot roundtrip**

Create `tests/id_correlation_test.rs`:

```rust
use chrono::Utc;
use pg_retest::correlate::sequence::SequenceState;
use pg_retest::profile::io;
use pg_retest::profile::{Metadata, Query, QueryKind, Session, WorkloadProfile};
use tempfile::NamedTempFile;

#[test]
fn test_sequence_snapshot_in_profile() {
    let snapshot = vec![
        SequenceState {
            schema: "public".into(),
            name: "orders_id_seq".into(),
            last_value: Some(42),
            increment_by: 1,
            start_value: 1,
            min_value: 1,
            max_value: i64::MAX,
            cycle: false,
            is_called: true,
        },
        SequenceState {
            schema: "public".into(),
            name: "users_id_seq".into(),
            last_value: None,
            increment_by: 1,
            start_value: 1,
            min_value: 1,
            max_value: i64::MAX,
            cycle: false,
            is_called: false,
        },
    ];

    let profile = WorkloadProfile {
        version: 2,
        captured_at: Utc::now(),
        source_host: "localhost".into(),
        pg_version: "16.2".into(),
        capture_method: "csv_log".into(),
        sessions: vec![Session {
            id: 1,
            user: "app".into(),
            database: "mydb".into(),
            queries: vec![Query {
                sql: "SELECT 1".into(),
                start_offset_us: 0,
                duration_us: 100,
                kind: QueryKind::Select,
                transaction_id: None,
                response_values: None,
            }],
        }],
        metadata: Metadata {
            total_queries: 1,
            total_sessions: 1,
            capture_duration_us: 100,
            sequence_snapshot: Some(snapshot.clone()),
            pk_map: None,
        },
    };

    let file = NamedTempFile::new().unwrap();
    io::write_profile(file.path(), &profile).unwrap();
    let loaded = io::read_profile(file.path()).unwrap();

    let loaded_snapshot = loaded.metadata.sequence_snapshot.unwrap();
    assert_eq!(loaded_snapshot.len(), 2);
    assert_eq!(loaded_snapshot[0].name, "orders_id_seq");
    assert_eq!(loaded_snapshot[0].last_value, Some(42));
    assert!(loaded_snapshot[0].is_called);
    assert_eq!(loaded_snapshot[1].last_value, None);
    assert!(!loaded_snapshot[1].is_called);
}

#[test]
fn test_profile_backward_compat() {
    // A profile written without the new fields should still load.
    // We test this by writing a profile with the current code (which includes
    // the new Option fields as None) and reading it back.
    let profile = WorkloadProfile {
        version: 2,
        captured_at: Utc::now(),
        source_host: "localhost".into(),
        pg_version: "16.2".into(),
        capture_method: "csv_log".into(),
        sessions: vec![Session {
            id: 1,
            user: "app".into(),
            database: "mydb".into(),
            queries: vec![Query {
                sql: "SELECT 1".into(),
                start_offset_us: 0,
                duration_us: 100,
                kind: QueryKind::Select,
                transaction_id: None,
                response_values: None,
            }],
        }],
        metadata: Metadata {
            total_queries: 1,
            total_sessions: 1,
            capture_duration_us: 100,
            sequence_snapshot: None,
            pk_map: None,
        },
    };

    let file = NamedTempFile::new().unwrap();
    io::write_profile(file.path(), &profile).unwrap();
    let loaded = io::read_profile(file.path()).unwrap();

    assert!(loaded.metadata.sequence_snapshot.is_none());
    assert!(loaded.metadata.pk_map.is_none());
    assert!(loaded.sessions[0].queries[0].response_values.is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test id_correlation_test 2>&1 | tail -10`
Expected: FAIL — `SequenceState` not found, `response_values` not a field, `sequence_snapshot` not a field.

- [ ] **Step 3: Implement `SequenceState` in `src/correlate/sequence.rs`**

```rust
use serde::{Deserialize, Serialize};

/// Snapshot of a single PostgreSQL sequence's state at capture time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceState {
    pub schema: String,
    pub name: String,
    /// None if nextval() has never been called on this sequence.
    pub last_value: Option<i64>,
    pub increment_by: i64,
    pub start_value: i64,
    pub min_value: i64,
    pub max_value: i64,
    pub cycle: bool,
    pub is_called: bool,
}

impl SequenceState {
    /// Returns the schema-qualified name for use in SQL (e.g., `"public"."orders_id_seq"`).
    pub fn qualified_name(&self) -> String {
        format!("\"{}\".\"{}\""  , self.schema, self.name)
    }

    /// Generates the `setval()` SQL to restore this sequence on a target database.
    pub fn restore_sql(&self) -> String {
        if self.is_called {
            if let Some(val) = self.last_value {
                format!("SELECT setval('{}', {}, true)", self.qualified_name(), val)
            } else {
                // is_called but last_value is None — shouldn't happen, but handle gracefully
                format!(
                    "SELECT setval('{}', {}, false)",
                    self.qualified_name(),
                    self.start_value
                )
            }
        } else {
            format!(
                "SELECT setval('{}', {}, false)",
                self.qualified_name(),
                self.start_value
            )
        }
    }
}
```

- [ ] **Step 4: Extend `Metadata` in `src/profile/mod.rs`**

Add imports at top of file:
```rust
use crate::correlate::capture::ResponseRow;
use crate::correlate::capture::TablePk;
use crate::correlate::sequence::SequenceState;
```

Add to `Query` struct (after `transaction_id` field, line 32):
```rust
    #[serde(default)]
    pub response_values: Option<Vec<ResponseRow>>,
```

Add to `Metadata` struct (after `capture_duration_us`, line 96):
```rust
    #[serde(default)]
    pub sequence_snapshot: Option<Vec<SequenceState>>,
    #[serde(default)]
    pub pk_map: Option<Vec<TablePk>>,
```

- [ ] **Step 5: Create stub types in `src/correlate/capture.rs`**

We need `ResponseRow` and `TablePk` to exist for the import. Create the file:

```rust
use serde::{Deserialize, Serialize};

/// A single row of captured RETURNING clause results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseRow {
    /// (column_name, text_value) pairs from the RETURNING result.
    pub columns: Vec<(String, String)>,
}

/// Primary key column mapping for a table, used by `--id-capture-implicit`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TablePk {
    pub schema: String,
    pub table: String,
    /// PK column names in ordinal order.
    pub columns: Vec<String>,
}
```

- [ ] **Step 6: Register `capture` submodule in `src/correlate/mod.rs`**

Add `pub mod capture;` after `pub mod sequence;`.

- [ ] **Step 7: Fix any compilation issues from the new fields**

Anywhere `Query { ... }` or `Metadata { ... }` is constructed without the new fields, the compiler will error. Add `response_values: None,` to all `Query` constructions and `sequence_snapshot: None, pk_map: None,` to all `Metadata` constructions.

Run: `cargo build 2>&1 | grep "error\[" | head -20` to find all locations.

**Full list of files that construct `Query {}` (add `response_values: None`):**
- `src/proxy/capture.rs` (build_profile + build_profile_from_staging)
- `src/capture/csv_log.rs`
- `src/capture/mysql_slow.rs`
- `src/capture/rds.rs`
- `src/transform/engine.rs`
- `src/transform/plan.rs`
- `src/transform/analyze.rs`
- `src/replay/scaling.rs`
- `tests/profile_io_test.rs`
- `tests/replay_test.rs`
- `tests/replay_e2e_test.rs`
- `tests/compare_test.rs`
- `tests/classify_test.rs`
- `tests/per_category_scaling_test.rs`
- `tests/pipeline_test.rs`
- `tests/scaling_test.rs`
- `tests/transform_test.rs`

**Full list of files that construct `Metadata {}` (add `sequence_snapshot: None, pk_map: None`):**
- `src/proxy/capture.rs`
- `src/transform/engine.rs`
- `src/transform/analyze.rs`
- `src/capture/csv_log.rs`
- `src/capture/mysql_slow.rs`
- `src/capture/rds.rs`
- `tests/profile_io_test.rs`
- `tests/compare_test.rs`
- `tests/classify_test.rs`
- `tests/per_category_scaling_test.rs`
- `tests/pipeline_test.rs`
- `tests/scaling_test.rs`
- `tests/transform_test.rs`

This is ~19 files total. Use `cargo build 2>&1 | grep "error\["` to catch any missed locations.

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test --test id_correlation_test 2>&1 | tail -10`
Expected: PASS — both tests pass.

Run: `cargo test 2>&1 | tail -5`
Expected: All existing tests still pass (backward compatibility).

- [ ] **Step 9: Commit**

```bash
git add src/correlate/sequence.rs src/correlate/capture.rs src/correlate/mod.rs src/profile/mod.rs tests/id_correlation_test.rs
# Also add all files touched for struct field fixes
git commit -m "feat(correlate): add SequenceState, ResponseRow, TablePk types and extend profile format"
```

---

### Task 3: Implement `snapshot_sequences()` and `restore_sequences()`

**Files:**
- Modify: `src/correlate/sequence.rs`

- [ ] **Step 1: Write unit tests for `SequenceState` methods**

Add to bottom of `src/correlate/sequence.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qualified_name() {
        let s = SequenceState {
            schema: "public".into(),
            name: "orders_id_seq".into(),
            last_value: Some(42),
            increment_by: 1,
            start_value: 1,
            min_value: 1,
            max_value: i64::MAX,
            cycle: false,
            is_called: true,
        };
        assert_eq!(s.qualified_name(), r#""public"."orders_id_seq""#);
    }

    #[test]
    fn test_restore_sql_called() {
        let s = SequenceState {
            schema: "public".into(),
            name: "orders_id_seq".into(),
            last_value: Some(42),
            increment_by: 1,
            start_value: 1,
            min_value: 1,
            max_value: i64::MAX,
            cycle: false,
            is_called: true,
        };
        assert_eq!(
            s.restore_sql(),
            r#"SELECT setval('"public"."orders_id_seq"', 42, true)"#
        );
    }

    #[test]
    fn test_restore_sql_not_called() {
        let s = SequenceState {
            schema: "public".into(),
            name: "orders_id_seq".into(),
            last_value: None,
            increment_by: 1,
            start_value: 1,
            min_value: 1,
            max_value: i64::MAX,
            cycle: false,
            is_called: false,
        };
        assert_eq!(
            s.restore_sql(),
            r#"SELECT setval('"public"."orders_id_seq"', 1, false)"#
        );
    }

    #[test]
    fn test_sequence_state_roundtrip() {
        let s = SequenceState {
            schema: "myschema".into(),
            name: "counter_seq".into(),
            last_value: Some(99999),
            increment_by: 10,
            start_value: 100,
            min_value: 1,
            max_value: 999999,
            cycle: true,
            is_called: true,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: SequenceState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.schema, "myschema");
        assert_eq!(back.last_value, Some(99999));
        assert!(back.cycle);
    }
}
```

- [ ] **Step 2: Run unit tests to verify they pass**

Run: `cargo test --lib correlate::sequence 2>&1 | tail -10`
Expected: PASS (4 tests).

- [ ] **Step 3: Implement `snapshot_sequences()` async function**

Add to `src/correlate/sequence.rs`:

```rust
use anyhow::{Context, Result};
use tokio_postgres::Client;
use tracing::{info, warn};

/// Snapshot all user-defined sequences from the source database.
/// Returns an empty Vec (not an error) if the query fails — graceful degradation.
pub async fn snapshot_sequences(client: &Client) -> Result<Vec<SequenceState>> {
    let rows = client
        .query(
            "SELECT schemaname, sequencename, last_value, increment_by, \
             start_value, min_value, max_value, cycle, \
             last_value IS NOT NULL AS is_called \
             FROM pg_sequences \
             WHERE schemaname NOT IN ('pg_catalog', 'information_schema')",
            &[],
        )
        .await
        .context("Failed to query pg_sequences for sequence snapshot")?;

    let mut sequences = Vec::with_capacity(rows.len());
    for row in &rows {
        sequences.push(SequenceState {
            schema: row.get("schemaname"),
            name: row.get("sequencename"),
            last_value: row.get("last_value"),
            increment_by: row.get("increment_by"),
            start_value: row.get("start_value"),
            min_value: row.get("min_value"),
            max_value: row.get("max_value"),
            cycle: row.get("cycle"),
            is_called: row.get("is_called"),
        });
    }

    info!(
        "Sequence snapshot captured: {} sequences",
        sequences.len()
    );
    Ok(sequences)
}

/// Restore sequences on the target database using setval().
/// Logs warnings for missing sequences but does not abort.
pub async fn restore_sequences(
    client: &Client,
    snapshot: &[SequenceState],
) -> (usize, usize, usize) {
    let mut reset = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;

    for seq in snapshot {
        let sql = seq.restore_sql();
        match client.simple_query(&sql).await {
            Ok(_) => {
                reset += 1;
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("does not exist") {
                    warn!(
                        "Sequence {} not found on target — skipping.",
                        seq.qualified_name()
                    );
                    skipped += 1;
                } else {
                    warn!(
                        "Failed to reset sequence {}: {}",
                        seq.qualified_name(),
                        msg
                    );
                    errors += 1;
                }
            }
        }
    }

    info!(
        "Sequence sync complete — {} reset, {} skipped, {} errors.",
        reset, skipped, errors
    );
    (reset, skipped, errors)
}
```

- [ ] **Step 4: Run all tests**

Run: `cargo test --lib correlate::sequence 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/correlate/sequence.rs
git commit -m "feat(correlate): implement snapshot_sequences() and restore_sequences()"
```

---

### Task 4: Wire Sequence Mode into Capture and Replay Entry Points

**Files:**
- Modify: `src/main.rs` (capture and replay command handlers)

- [ ] **Step 1: Read `src/main.rs` to find the capture and replay entry points**

Find where `Commands::Capture(args)` and `Commands::Replay(args)` are handled. Identify where the DB connection is established (if any) and where `write_profile()` and `run_replay()` are called.

- [ ] **Step 2: Wire sequence snapshot into proxy capture**

In the proxy startup path (where the proxy begins accepting connections), add before the accept loop:

```rust
let sequence_snapshot = if args.id_mode.needs_sequences() {
    // Connect to target to snapshot sequences
    let (client, connection) = tokio_postgres::connect(&args.target, NoTls).await?;
    tokio::spawn(async move { let _ = connection.await; });
    match correlate::sequence::snapshot_sequences(&client).await {
        Ok(snap) => Some(snap),
        Err(e) => {
            warn!("Sequence snapshot failed: {e}. Continuing without snapshot.");
            None
        }
    }
} else {
    None
};
```

Then pass `sequence_snapshot` into the profile metadata when building the profile after capture completes.

- [ ] **Step 3: Wire sequence restore into replay**

In the replay path, before calling `run_replay()`:

```rust
if args.id_mode.needs_sequences() {
    if let Some(ref snapshot) = profile.metadata.sequence_snapshot {
        let (client, connection) = tokio_postgres::connect(&connection_string, NoTls).await?;
        tokio::spawn(async move { let _ = connection.await; });
        correlate::sequence::restore_sequences(&client, snapshot).await;
    } else {
        warn!("--id-mode requires sequence snapshot but workload has none. Skipping sequence restore.");
    }
}
```

- [ ] **Step 4: Wire sequence snapshot into log-based capture (graceful degradation)**

In the `Commands::Capture` path, if `args.id_mode.needs_sequences()` but there is no live DB connection available (log-based capture without source host flags), emit:

```rust
warn!("--id-mode=sequence requires a live connection to the source database. Sequence snapshot skipped.");
```

- [ ] **Step 5: Verify it compiles and existing tests pass**

Run: `cargo build && cargo test 2>&1 | tail -5`
Expected: Build and all tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "feat(correlate): wire sequence snapshot into capture and restore into replay"
```

---

### Task 5: Phase 1 Documentation

**Files:**
- Modify: `src/cli.rs` (help text)
- Create: `docs/id-correlation.md`

- [ ] **Step 1: Add detailed help text to `--id-mode` arguments**

Update the `#[arg(...)]` attribute on all three `id_mode` fields to include a `help` string:

```rust
    /// ID handling mode for database-generated values (sequences, UUIDs).
    /// none: no handling (default). sequence: snapshot sequences at capture,
    /// reset before replay. correlate: capture RETURNING values via proxy,
    /// remap during replay (requires proxy capture). full: both combined.
    #[arg(long, value_enum, default_value_t = crate::correlate::IdMode::None)]
    pub id_mode: crate::correlate::IdMode,
```

- [ ] **Step 2: Create `docs/id-correlation.md`**

Write a user-facing doc explaining the problem, the four modes, usage examples, and known limitations. Reference the Known Limitations section from the spec.

- [ ] **Step 3: Commit**

```bash
git add src/cli.rs docs/id-correlation.md
git commit -m "docs: add ID correlation documentation and help text"
```

---

## Phase 2: Correlate Mode

### Task 6: Add `dashmap` Dependency

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add dashmap to dependencies**

Add under `[dependencies]`:
```toml
dashmap = "6"
```

- [ ] **Step 2: Add criterion to dev-dependencies and bench target**

Add under `[dev-dependencies]`:
```toml
criterion = { version = "0.5", features = ["html_reports"] }
```

Add at bottom of `Cargo.toml`:
```toml
[[bench]]
name = "substitute_bench"
harness = false
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build 2>&1 | tail -3`
Expected: Build succeeds.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml
git commit -m "build: add dashmap and criterion dependencies"
```

---

### Task 7: Implement `IdMap` (`src/correlate/map.rs`)

**Files:**
- Create: `src/correlate/map.rs`
- Modify: `src/correlate/mod.rs` (add `pub mod map;`)

- [ ] **Step 1: Write failing tests for IdMap**

Create `src/correlate/map.rs` with tests at the bottom:

```rust
use std::borrow::Cow;
use std::sync::Arc;

use dashmap::DashMap;

/// Global shared ID map for cross-session correlation.
/// Maps captured source values to replay-generated values.
pub struct IdMap {
    inner: Arc<DashMap<String, String>>,
}

impl Clone for IdMap {
    fn clone(&self) -> Self {
        IdMap {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl IdMap {
    pub fn new() -> Self {
        IdMap {
            inner: Arc::new(DashMap::new()),
        }
    }

    /// Register a captured→replayed value mapping.
    /// Only call when captured != replayed.
    pub fn register(&self, captured: String, replayed: String) {
        self.inner.insert(captured, replayed);
    }

    /// Look up a captured value and return its replay equivalent.
    pub fn get(&self, captured: &str) -> Option<String> {
        self.inner.get(captured).map(|v| v.value().clone())
    }

    /// Number of registered mappings.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Substitute all known captured IDs in the SQL with their replay equivalents.
    /// Returns Cow::Borrowed if no substitutions were made (zero allocation).
    pub fn substitute<'a>(&self, sql: &'a str) -> (Cow<'a, str>, usize) {
        if self.inner.is_empty() {
            return (Cow::Borrowed(sql), 0);
        }
        super::substitute::substitute_ids(sql, &self.inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_lookup() {
        let map = IdMap::new();
        map.register("42".into(), "1001".into());
        assert_eq!(map.get("42"), Some("1001".into()));
        assert_eq!(map.get("99"), None);
    }

    #[test]
    fn test_map_len() {
        let map = IdMap::new();
        assert_eq!(map.len(), 0);
        assert!(map.is_empty());
        map.register("42".into(), "1001".into());
        map.register("43".into(), "1002".into());
        assert_eq!(map.len(), 2);
        assert!(!map.is_empty());
    }

    #[test]
    fn test_clone_shares_state() {
        let map1 = IdMap::new();
        let map2 = map1.clone();
        map1.register("42".into(), "1001".into());
        assert_eq!(map2.get("42"), Some("1001".into()));
    }

    #[tokio::test]
    async fn test_concurrent_register() {
        let map = IdMap::new();
        let mut handles = Vec::new();
        for task_id in 0..10u64 {
            let m = map.clone();
            handles.push(tokio::spawn(async move {
                for i in 0..100u64 {
                    let key = format!("{}_{}", task_id, i);
                    let val = format!("new_{}_{}", task_id, i);
                    m.register(key, val);
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(map.len(), 1000);
    }
}
```

- [ ] **Step 2: Register module**

Add `pub mod map;` to `src/correlate/mod.rs`.

- [ ] **Step 3: Create stub `src/correlate/substitute.rs`**

```rust
use std::borrow::Cow;
use dashmap::DashMap;

/// Substitute captured ID values in SQL with their replay equivalents.
/// Returns the (possibly modified) SQL and the count of substitutions made.
pub fn substitute_ids<'a>(sql: &'a str, _map: &DashMap<String, String>) -> (Cow<'a, str>, usize) {
    // Stub — implemented in Task 8
    (Cow::Borrowed(sql), 0)
}
```

Add `pub mod substitute;` to `src/correlate/mod.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test --lib correlate::map 2>&1 | tail -10`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add src/correlate/map.rs src/correlate/substitute.rs src/correlate/mod.rs
git commit -m "feat(correlate): implement IdMap with DashMap and concurrent tests"
```

---

### Task 8: Implement SQL Substitution State Machine

**Files:**
- Modify: `src/correlate/substitute.rs`

This is the most complex and thoroughly tested component. Read `src/capture/masking.rs` first for the existing state machine pattern.

- [ ] **Step 1: Write failing unit tests**

Replace the stub `src/correlate/substitute.rs` with the full implementation file including all tests listed in the spec. The tests go in a `#[cfg(test)] mod tests` block at the bottom.

Key tests (write ALL of these):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use dashmap::DashMap;

    fn make_map(entries: &[(&str, &str)]) -> DashMap<String, String> {
        let map = DashMap::new();
        for (k, v) in entries {
            map.insert(k.to_string(), v.to_string());
        }
        map
    }

    #[test]
    fn test_integer_in_where() {
        let map = make_map(&[("42", "1001")]);
        let (result, count) = substitute_ids("SELECT * FROM t WHERE id = 42", &map);
        assert_eq!(result, "SELECT * FROM t WHERE id = 1001");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_no_substitute_in_limit() {
        let map = make_map(&[("42", "1001")]);
        let (result, _) = substitute_ids("SELECT * FROM t LIMIT 42", &map);
        assert_eq!(result, "SELECT * FROM t LIMIT 42");
    }

    #[test]
    fn test_no_substitute_partial_match() {
        let map = make_map(&[("42", "1001")]);
        let (result, count) = substitute_ids("SELECT * FROM t WHERE id = 420", &map);
        assert_eq!(result, "SELECT * FROM t WHERE id = 420");
        assert_eq!(count, 0);
    }

    #[test]
    fn test_no_substitute_in_string() {
        let map = make_map(&[("42", "1001")]);
        let (result, count) = substitute_ids("SELECT * FROM t WHERE name = 'item42'", &map);
        assert_eq!(result, "SELECT * FROM t WHERE name = 'item42'");
        assert_eq!(count, 0);
    }

    #[test]
    fn test_uuid_in_where() {
        let uuid_src = "550e8400-e29b-41d4-a716-446655440000";
        let uuid_dst = "aaaabbbb-cccc-dddd-eeee-ffffffffffff";
        let map = make_map(&[(uuid_src, uuid_dst)]);
        let sql = format!("SELECT * FROM t WHERE uuid = '{}'", uuid_src);
        let (result, count) = substitute_ids(&sql, &map);
        assert_eq!(result, format!("SELECT * FROM t WHERE uuid = '{}'", uuid_dst));
        assert_eq!(count, 1);
    }

    #[test]
    fn test_no_map_entries() {
        let map = DashMap::new();
        let sql = "SELECT * FROM t WHERE id = 42";
        let (result, count) = substitute_ids(sql, &map);
        assert_eq!(result, sql);
        assert_eq!(count, 0);
    }

    #[test]
    fn test_multiple_substitutions() {
        let map = make_map(&[("42", "1001"), ("43", "1002"), ("44", "1003")]);
        let (result, count) = substitute_ids(
            "SELECT * FROM t WHERE id IN (42, 43, 44)",
            &map,
        );
        assert_eq!(result, "SELECT * FROM t WHERE id IN (1001, 1002, 1003)");
        assert_eq!(count, 3);
    }

    #[test]
    fn test_no_substitute_in_identifier() {
        let map = make_map(&[("42", "1001")]);
        let (result, count) = substitute_ids("SELECT col42 FROM t", &map);
        assert_eq!(result, "SELECT col42 FROM t");
        assert_eq!(count, 0);
    }

    #[test]
    fn test_integer_in_values() {
        let map = make_map(&[("42", "1001")]);
        let (result, count) = substitute_ids(
            "INSERT INTO t (id, name) VALUES (42, 'foo')",
            &map,
        );
        assert_eq!(result, "INSERT INTO t (id, name) VALUES (1001, 'foo')");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_no_substitute_in_offset() {
        let map = make_map(&[("10", "999")]);
        let (result, _) = substitute_ids("SELECT * FROM t LIMIT 5 OFFSET 10", &map);
        assert_eq!(result, "SELECT * FROM t LIMIT 5 OFFSET 10");
    }

    #[test]
    fn test_dollar_quoted_string() {
        let map = make_map(&[("42", "1001")]);
        let (result, count) = substitute_ids("SELECT $$contains 42$$", &map);
        assert_eq!(result, "SELECT $$contains 42$$");
        assert_eq!(count, 0);
    }

    #[test]
    fn test_escaped_quotes() {
        let map = make_map(&[("42", "1001")]);
        let (result, count) = substitute_ids("WHERE name = 'it''s 42'", &map);
        // 42 is inside a string literal — not substituted
        assert_eq!(result, "WHERE name = 'it''s 42'");
        assert_eq!(count, 0);
    }

    #[test]
    fn test_subquery_with_limit() {
        let map = make_map(&[("42", "1001"), ("5", "999"), ("99", "2002")]);
        let sql = "SELECT * FROM t WHERE id = 42 AND status IN (SELECT s FROM t2 LIMIT 5) AND x = 99";
        let (result, count) = substitute_ids(sql, &map);
        // 42 → 1001 (WHERE), 5 unchanged (LIMIT), 99 → 2002 (AND)
        assert_eq!(
            result,
            "SELECT * FROM t WHERE id = 1001 AND status IN (SELECT s FROM t2 LIMIT 5) AND x = 2002"
        );
        assert_eq!(count, 2);
    }

    #[test]
    fn test_integer_in_set() {
        let map = make_map(&[("42", "1001")]);
        let (result, count) = substitute_ids("UPDATE t SET order_id = 42 WHERE 1=1", &map);
        assert_eq!(result, "UPDATE t SET order_id = 1001 WHERE 1=1");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_eligibility_resets_after_literal() {
        let map = make_map(&[("5", "999"), ("42", "1001")]);
        let sql = "SELECT * FROM t LIMIT 5 WHERE id = 42";
        let (result, _) = substitute_ids(sql, &map);
        // 5 not substituted (LIMIT context), 42 substituted (WHERE context)
        assert!(result.contains("LIMIT 5"));
        assert!(result.contains("= 1001"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib correlate::substitute 2>&1 | tail -10`
Expected: FAIL — stub always returns Cow::Borrowed, substitution tests fail.

- [ ] **Step 3: Implement the state machine**

Replace the stub `substitute_ids` function with the full implementation. Follow the algorithm from the spec:

1. Character-level walk tracking states: `Normal`, `InStringLiteral`, `InIdentifier`, `InLineComment`, `InBlockComment`, `InNumericLiteral`, `InDollarQuote`
2. Keyword tracking with single-literal eligibility scope
3. Standalone check for numeric literals (not preceded/followed by alphanumeric or underscore)
4. String literal content lookup in map
5. DashMap `.get()` for O(1) lookup per literal

This is ~150-250 lines of Rust. Study `src/capture/masking.rs` for the character iteration pattern.

- [ ] **Step 4: Run all substitute tests**

Run: `cargo test --lib correlate::substitute 2>&1 | tail -20`
Expected: ALL tests pass.

- [ ] **Step 5: Run full test suite for regressions**

Run: `cargo test 2>&1 | tail -5`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/correlate/substitute.rs
git commit -m "feat(correlate): implement SQL substitution state machine with positional context filtering"
```

---

### Task 9: Add Protocol Parsers (`extract_row_description`, `extract_data_row`)

**Files:**
- Modify: `src/proxy/protocol.rs`

- [ ] **Step 1: Write tests for the new parsers**

Add to the existing `#[cfg(test)]` module in `protocol.rs` (or create one if it doesn't exist):

```rust
#[test]
fn test_extract_row_description_single_column() {
    // Build a RowDescription message with one column "id"
    let mut body = Vec::new();
    body.extend_from_slice(&1u16.to_be_bytes()); // 1 column
    // Column: "id\0", table_oid(4), col_num(2), type_oid(4), type_size(2), type_mod(4), format(2)
    body.extend_from_slice(b"id\0");
    body.extend_from_slice(&0u32.to_be_bytes()); // table OID
    body.extend_from_slice(&0u16.to_be_bytes()); // column number
    body.extend_from_slice(&23u32.to_be_bytes()); // type OID (int4)
    body.extend_from_slice(&4i16.to_be_bytes()); // type size
    body.extend_from_slice(&(-1i32).to_be_bytes()); // type modifier
    body.extend_from_slice(&0u16.to_be_bytes()); // format code (text)

    let msg = PgMessage {
        msg_type: b'T',
        length: (body.len() + 4) as u32,
        payload: {
            let mut p = Vec::new();
            p.extend_from_slice(&((body.len() + 4) as u32).to_be_bytes());
            p.extend_from_slice(&body);
            p
        },
    };
    let cols = extract_row_description(&msg).unwrap();
    assert_eq!(cols, vec!["id".to_string()]);
}

#[test]
fn test_extract_data_row_single_value() {
    // Build a DataRow with one column value "42"
    let mut body = Vec::new();
    body.extend_from_slice(&1u16.to_be_bytes()); // 1 column
    let val = b"42";
    body.extend_from_slice(&(val.len() as i32).to_be_bytes()); // length
    body.extend_from_slice(val);

    let msg = PgMessage {
        msg_type: b'D',
        length: (body.len() + 4) as u32,
        payload: {
            let mut p = Vec::new();
            p.extend_from_slice(&((body.len() + 4) as u32).to_be_bytes());
            p.extend_from_slice(&body);
            p
        },
    };
    let values = extract_data_row(&msg, 1).unwrap();
    assert_eq!(values, vec![Some("42".to_string())]);
}
```

- [ ] **Step 2: Implement `extract_row_description()`**

```rust
/// Parse RowDescription ('T') message — returns column names.
/// Body: Int16 (num columns), then per column:
///   String (name), Int32 (table OID), Int16 (col num), Int32 (type OID),
///   Int16 (type size), Int32 (type mod), Int16 (format code)
pub fn extract_row_description(msg: &PgMessage) -> Option<Vec<String>> {
    if msg.msg_type != b'T' {
        return None;
    }
    let body = msg.body();
    if body.len() < 2 {
        return None;
    }
    let num_cols = u16::from_be_bytes([body[0], body[1]]) as usize;
    let mut columns = Vec::with_capacity(num_cols);
    let mut pos = 2;
    for _ in 0..num_cols {
        // Read null-terminated column name
        let name_end = body[pos..].iter().position(|&b| b == 0)?;
        let name = String::from_utf8_lossy(&body[pos..pos + name_end]).into_owned();
        pos += name_end + 1; // skip null terminator
        // Skip: table OID (4) + col num (2) + type OID (4) + type size (2) + type mod (4) + format (2) = 18 bytes
        pos += 18;
        columns.push(name);
    }
    Some(columns)
}
```

- [ ] **Step 3: Implement `extract_data_row()`**

```rust
/// Parse DataRow ('D') message — returns column values as text strings.
/// Body: Int16 (num columns), then per column: Int32 (length, -1 for NULL), bytes (value)
pub fn extract_data_row(msg: &PgMessage, num_columns: usize) -> Option<Vec<Option<String>>> {
    if msg.msg_type != b'D' {
        return None;
    }
    let body = msg.body();
    if body.len() < 2 {
        return None;
    }
    let num_cols = u16::from_be_bytes([body[0], body[1]]) as usize;
    if num_cols != num_columns {
        return None;
    }
    let mut values = Vec::with_capacity(num_cols);
    let mut pos = 2;
    for _ in 0..num_cols {
        if pos + 4 > body.len() {
            return None;
        }
        let len = i32::from_be_bytes([body[pos], body[pos + 1], body[pos + 2], body[pos + 3]]);
        pos += 4;
        if len == -1 {
            values.push(None); // NULL
        } else {
            let len = len as usize;
            if pos + len > body.len() {
                return None;
            }
            let val = String::from_utf8_lossy(&body[pos..pos + len]).into_owned();
            pos += len;
            values.push(Some(val));
        }
    }
    Some(values)
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib proxy::protocol 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/proxy/protocol.rs
git commit -m "feat(proxy): add extract_row_description() and extract_data_row() protocol parsers"
```

---

### Task 10: Add RETURNING Detection + DataRow Capture in Proxy Relay

**Files:**
- Modify: `src/proxy/connection.rs`
- Modify: `src/proxy/capture.rs`
- Modify: `src/correlate/capture.rs`

- [ ] **Step 1: Add `has_returning()` to `src/correlate/capture.rs`**

```rust
/// Check if SQL contains a RETURNING clause (not inside a string literal).
/// Uses a simplified state machine to avoid matching RETURNING inside strings.
pub fn has_returning(sql: &str) -> bool {
    let upper = sql.to_uppercase();
    // Quick check before expensive parsing
    if !upper.contains("RETURNING") {
        return false;
    }
    // Verify it's not inside a string literal
    let mut in_string = false;
    let chars: Vec<char> = sql.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\'' {
            if in_string && i + 1 < chars.len() && chars[i + 1] == '\'' {
                i += 2; // escaped quote
                continue;
            }
            in_string = !in_string;
        } else if !in_string && i + 9 <= chars.len() {
            let chunk: String = chars[i..i + 9].iter().collect();
            if chunk.eq_ignore_ascii_case("RETURNING") {
                // Check it's a keyword boundary (not part of a longer word)
                let before_ok = i == 0 || !chars[i - 1].is_alphanumeric();
                let after_ok =
                    i + 9 >= chars.len() || !chars[i + 9].is_alphanumeric();
                if before_ok && after_ok {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}
```

Add unit tests for `has_returning()`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_returning_simple() {
        assert!(has_returning("INSERT INTO t (x) VALUES (1) RETURNING id"));
    }

    #[test]
    fn test_detect_returning_case_insensitive() {
        assert!(has_returning("INSERT INTO t (x) VALUES (1) returning id"));
        assert!(has_returning("INSERT INTO t (x) VALUES (1) Returning id"));
    }

    #[test]
    fn test_no_returning() {
        assert!(!has_returning("INSERT INTO t (x) VALUES (1)"));
        assert!(!has_returning("SELECT * FROM t"));
        assert!(!has_returning("UPDATE t SET x = 1"));
    }

    #[test]
    fn test_returning_in_string_literal() {
        assert!(!has_returning(
            "INSERT INTO t (note) VALUES ('use RETURNING for ids')"
        ));
    }
}
```

- [ ] **Step 2: Add `CorrelateState` struct and RETURNING queue to `connection.rs`**

Add shared state struct (created in `handle_connection_inner`, passed to both relay tasks):

```rust
use std::collections::VecDeque;
use tokio::sync::Mutex;

struct CorrelateState {
    returning_queue: Mutex<VecDeque<bool>>,
    pending_columns: Mutex<Vec<String>>,
    pending_rows: Mutex<Vec<Vec<Option<String>>>>,
}
```

In `relay_client_to_server`, after detecting SQL (both `'Q'` and `'P'` paths), push to the returning queue:

```rust
if let Some(ref correlate) = correlate_state {
    let has_ret = crate::correlate::capture::has_returning(&sql);
    correlate.returning_queue.lock().await.push_back(has_ret);
}
```

In `relay_server_to_client`, add arms for `'T'` and `'D'` when correlate_state is Some:

```rust
b'T' => {
    if let Some(ref cs) = correlate_state {
        if let Some(true) = cs.returning_queue.lock().await.front() {
            if let Some(cols) = protocol::extract_row_description(&msg) {
                if cols.len() <= 20 {
                    *cs.pending_columns.lock().await = cols;
                }
            }
        }
    }
}
b'D' => {
    if let Some(ref cs) = correlate_state {
        let pending_cols = cs.pending_columns.lock().await;
        if !pending_cols.is_empty() {
            let num_cols = pending_cols.len();
            drop(pending_cols); // release lock before next lock
            if let Some(values) = protocol::extract_data_row(&msg, num_cols) {
                let mut rows = cs.pending_rows.lock().await;
                if rows.len() < 100 {
                    rows.push(values);
                }
            }
        }
    }
}
```

On `'C'` (CommandComplete), after existing handling, drain the RETURNING state:

```rust
if let Some(ref cs) = correlate_state {
    let was_returning = cs.returning_queue.lock().await.pop_front().unwrap_or(false);
    if was_returning {
        let columns = std::mem::take(&mut *cs.pending_columns.lock().await);
        let rows = std::mem::take(&mut *cs.pending_rows.lock().await);
        if !rows.is_empty() {
            let event = CaptureEvent::QueryReturning {
                session_id,
                columns,
                rows,
                timestamp: Instant::now(),
            };
            if let Some(ref mtx) = metrics_tx {
                let _ = mtx.send(event.clone());
            }
            let _ = capture_tx.send(event);
        }
    }
}
```

- [ ] **Step 3: Add `CaptureEvent::QueryReturning` to `capture.rs`**

Add the new variant to the enum:

```rust
QueryReturning {
    session_id: u64,
    columns: Vec<String>,
    rows: Vec<Vec<Option<String>>>,
    timestamp: Instant,
},
```

Handle it in `run_collector` and `run_staging_collector` — associate with the most recent pending query for that session:

```rust
CaptureEvent::QueryReturning { session_id, columns, rows, .. } => {
    if let Some(state) = sessions.get_mut(&session_id) {
        if let Some(last_query) = state.queries.last_mut() {
            let response_values: Vec<ResponseRow> = rows.iter().map(|row| {
                ResponseRow {
                    columns: columns.iter().zip(row.iter()).map(|(name, val)| {
                        (name.clone(), val.clone().unwrap_or_default())
                    }).collect()
                }
            }).collect();
            last_query.response_values = Some(response_values);
        }
    }
}
```

Add `response_values: Option<Vec<ResponseRow>>` field to `CapturedQuery` struct, defaulting to `None`.

- [ ] **Step 4: Update `StagingRow` and SQLite schema for response values**

Modify `src/proxy/staging.rs`:

Add field to `StagingRow`:
```rust
    pub response_values_json: Option<String>,  // JSON-encoded Vec<ResponseRow>
```

Add column to the `CREATE TABLE` SQL:
```sql
    response_values_json TEXT
```

Update `insert_batch()` to include the new column.

Update `build_profile_from_staging()` to deserialize `response_values_json` back into `Option<Vec<ResponseRow>>` using `serde_json::from_str()`.

- [ ] **Step 5: Update `build_profile()` to propagate `response_values`**

In `src/proxy/capture.rs::build_profile()`, when constructing `Query` from `CapturedQuery`, propagate the field:
```rust
response_values: cq.response_values,
```

Same for `build_profile_from_staging()`.

- [ ] **Step 6: Verify it compiles**

Run: `cargo build 2>&1 | tail -10`

- [ ] **Step 7: Run all tests**

Run: `cargo test 2>&1 | tail -5`
Expected: All tests pass.

- [ ] **Step 8: Run `cargo fmt`**

Run: `cargo fmt`

- [ ] **Step 9: Commit**

```bash
git add src/proxy/connection.rs src/proxy/capture.rs src/proxy/staging.rs src/correlate/capture.rs
git commit -m "feat(proxy): add RETURNING detection and DataRow capture in proxy relay"
```

---

### Task 11: Wire Correlation into Replay Engine

**Files:**
- Modify: `src/replay/mod.rs`
- Modify: `src/replay/session.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Add `id_substitution_count` to `QueryResult`**

In `src/replay/mod.rs`, add to `QueryResult`:
```rust
    #[serde(default)]
    pub id_substitution_count: usize,
```

- [ ] **Step 2: Accept `Option<IdMap>` in `replay_session()`**

Modify `src/replay/session.rs::replay_session()` signature to accept `id_map: Option<IdMap>`:

```rust
pub async fn replay_session(
    session: &Session,
    connection_string: &str,
    mode: ReplayMode,
    speed: f64,
    replay_start: TokioInstant,
    tls: Option<MakeRustlsConnect>,
    id_map: Option<crate::correlate::map::IdMap>,
) -> Result<ReplayResults> {
```

- [ ] **Step 3: Add substitution + registration to the query loop**

In the `for query in &session.queries` loop, before `client.simple_query`:

```rust
        // ID substitution
        let (effective_sql, sub_count) = match &id_map {
            Some(map) => {
                let (sql, count) = map.substitute(&query.sql);
                (sql.into_owned(), count)
            }
            None => (query.sql.clone(), 0),
        };

        let start = Instant::now();
        let result = client.simple_query(&effective_sql).await;
        let elapsed_us = start.elapsed().as_micros() as u64;
```

After the result, register RETURNING value mappings:

```rust
        // Register ID mappings from RETURNING results
        if let (Ok(ref messages), Some(ref map), Some(ref captured_rows)) =
            (&result, &id_map, &query.response_values)
        {
            use tokio_postgres::SimpleQueryMessage;
            for msg in messages {
                if let SimpleQueryMessage::Row(row) = msg {
                    // Find matching captured row (first unmatched)
                    // For simplicity, match sequentially
                    for captured in captured_rows {
                        for (idx, (col_name, captured_val)) in captured.columns.iter().enumerate() {
                            if let Some(replay_val) = row.try_get(idx).ok().flatten() {
                                if replay_val != captured_val {
                                    map.register(captured_val.clone(), replay_val.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
```

Update `QueryResult` construction:
```rust
        query_results.push(QueryResult {
            sql: effective_sql,
            original_duration_us: query.duration_us,
            replay_duration_us: elapsed_us,
            success,
            error,
            id_substitution_count: sub_count,
        });
```

- [ ] **Step 4: Accept `Option<IdMap>` in `run_replay()`**

In `src/replay/mod.rs`, modify `run_replay()` to create the IdMap and pass it to each session:

```rust
pub async fn run_replay(
    profile: &WorkloadProfile,
    connection_string: &str,
    mode: ReplayMode,
    speed: f64,
    tls: Option<MakeRustlsConnect>,
    id_mode: crate::correlate::IdMode,
    // ... other params
) -> Result<Vec<ReplayResults>> {
    let id_map = if id_mode.needs_correlation() {
        Some(crate::correlate::map::IdMap::new())
    } else {
        None
    };

    // Pass id_map.clone() to each spawned session task
    // ...
}
```

- [ ] **Step 5: Wire `id_mode` through `main.rs`**

Pass `args.id_mode` from the CLI to `run_replay()`. If `id_mode.needs_correlation()` and the profile's `capture_method` is not `"proxy"`, emit an error:

```rust
if args.id_mode.needs_correlation() && profile.capture_method != "proxy" {
    anyhow::bail!(
        "--id-mode=correlate requires proxy-captured workload (found: {}). \
         Re-capture using `pg-retest proxy` or use --id-mode=sequence instead.",
        profile.capture_method
    );
}
```

- [ ] **Step 6: Update ALL callers of `run_replay()` and `replay_session()`**

The signature changes affect more than just `main.rs`. Update each caller to pass `IdMode::None` (or the appropriate mode):

**`run_replay()` callers (add `id_mode` parameter):**
- `src/main.rs` — pass `args.id_mode`
- `src/tuner/mod.rs` — pass `IdMode::None` (tuner manages its own connection)
- `src/pipeline/mod.rs` — pass `IdMode::None` (or wire through pipeline config)
- `src/web/handlers/ab.rs` — pass `IdMode::None`
- `src/web/handlers/demo.rs` — pass `IdMode::None`

**`replay_session()` callers (add `id_map` parameter):**
- `src/web/handlers/replay.rs` — pass `None`

- [ ] **Step 7: Update ALL `QueryResult {}` construction sites**

Add `id_substitution_count: 0` to every `QueryResult` construction outside `replay/session.rs`:
- `src/compare/mod.rs` (~3 constructions)
- `tests/replay_test.rs` (~2 constructions)
- `tests/compare_test.rs` (multiple constructions)
- `tests/ab_test.rs`
- `tests/scaling_test.rs` (~3 constructions)

Use `cargo build 2>&1 | grep "id_substitution_count"` to find any missed sites.

- [ ] **Step 8: Verify compilation and all tests**

Run: `cargo build && cargo test 2>&1 | tail -5`
Expected: Build and all tests pass.

- [ ] **Step 9: Commit**

```bash
git add src/replay/mod.rs src/replay/session.rs src/main.rs src/tuner/mod.rs src/pipeline/mod.rs src/web/handlers/ src/compare/mod.rs tests/
git commit -m "feat(correlate): wire ID substitution and registration into replay engine"
```

---

### Task 12: Implement `--id-capture-implicit` (Auto-inject RETURNING + currval/lastval)

**Files:**
- Modify: `src/correlate/capture.rs`
- Modify: `src/cli.rs`
- Modify: `src/proxy/connection.rs`

- [ ] **Step 1: Add `--id-capture-implicit` and `--id-correlate-all-columns` flags to CLI**

Add to `ProxyArgs` and `ReplayArgs`:

```rust
    /// Auto-inject RETURNING for bare INSERTs and intercept currval/lastval
    #[arg(long, default_value_t = false)]
    pub id_capture_implicit: bool,

    /// Register all differing RETURNING column values (not just integers/UUIDs)
    #[arg(long, default_value_t = false)]
    pub id_correlate_all_columns: bool,
```

- [ ] **Step 2: Add PK discovery function to `src/correlate/capture.rs`**

```rust
/// Discover primary key columns for all user tables.
pub async fn discover_primary_keys(client: &tokio_postgres::Client) -> Result<Vec<TablePk>> {
    let rows = client
        .query(
            "SELECT kcu.table_schema, kcu.table_name, kcu.column_name, kcu.ordinal_position \
             FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu \
                 USING (constraint_schema, constraint_name, table_schema, table_name) \
             WHERE tc.constraint_type = 'PRIMARY KEY' \
                 AND tc.table_schema NOT IN ('pg_catalog', 'information_schema') \
             ORDER BY kcu.table_schema, kcu.table_name, kcu.ordinal_position",
            &[],
        )
        .await
        .context("Failed to discover primary keys")?;

    let mut pk_map: std::collections::BTreeMap<(String, String), Vec<String>> =
        std::collections::BTreeMap::new();
    for row in &rows {
        let schema: String = row.get(0);
        let table: String = row.get(1);
        let column: String = row.get(2);
        pk_map.entry((schema, table)).or_default().push(column);
    }

    Ok(pk_map
        .into_iter()
        .map(|((schema, table), columns)| TablePk { schema, table, columns })
        .collect())
}
```

- [ ] **Step 3: Add `inject_returning()` function**

```rust
/// If SQL is a bare INSERT (no RETURNING) targeting a known PK table, append RETURNING.
pub fn inject_returning(sql: &str, pk_map: &[TablePk]) -> Option<String> {
    if has_returning(sql) {
        return None; // Already has RETURNING
    }
    let upper = sql.trim_start().to_uppercase();
    if !upper.starts_with("INSERT") {
        return None;
    }
    // Extract table name from INSERT INTO <table>
    // Simple regex-free approach: find "INTO" keyword, next word is the table
    let upper_chars: Vec<char> = upper.chars().collect();
    let into_pos = upper.find("INTO ")?;
    let after_into = &sql[into_pos + 5..].trim_start();
    let table_name: String = after_into
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.' || *c == '"')
        .collect();

    // Find matching PK
    let table_clean = table_name.replace('"', "");
    let pk = pk_map.iter().find(|pk| {
        let qualified = format!("{}.{}", pk.schema, pk.table);
        table_clean == pk.table || table_clean == qualified
    })?;

    let returning_cols = pk.columns.join(", ");
    let trimmed = sql.trim_end().trim_end_matches(';');
    Some(format!("{} RETURNING {}", trimmed, returning_cols))
}

/// Detect if a query is SELECT currval(...) or SELECT lastval().
pub fn is_currval_or_lastval(sql: &str) -> bool {
    let upper = sql.trim_start().to_uppercase();
    (upper.starts_with("SELECT") && (upper.contains("CURRVAL") || upper.contains("LASTVAL")))
}
```

Add unit tests:
```rust
    #[test]
    fn test_inject_returning() {
        let pk_map = vec![TablePk {
            schema: "public".into(),
            table: "orders".into(),
            columns: vec!["id".into()],
        }];
        let result = inject_returning("INSERT INTO orders (name) VALUES ('test')", &pk_map);
        assert_eq!(
            result,
            Some("INSERT INTO orders (name) VALUES ('test') RETURNING id".into())
        );
    }

    #[test]
    fn test_inject_returning_already_has_returning() {
        let pk_map = vec![TablePk {
            schema: "public".into(),
            table: "orders".into(),
            columns: vec!["id".into()],
        }];
        let result = inject_returning(
            "INSERT INTO orders (name) VALUES ('test') RETURNING id",
            &pk_map,
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_inject_returning_unknown_table() {
        let pk_map = vec![TablePk {
            schema: "public".into(),
            table: "orders".into(),
            columns: vec!["id".into()],
        }];
        let result = inject_returning(
            "INSERT INTO unknown_table (name) VALUES ('test')",
            &pk_map,
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_detect_currval() {
        assert!(is_currval_or_lastval("SELECT currval('orders_id_seq')"));
        assert!(is_currval_or_lastval("SELECT lastval()"));
        assert!(!is_currval_or_lastval("SELECT * FROM orders"));
    }
```

- [ ] **Step 4: Wire implicit capture into proxy relay**

In `relay_client_to_server`, when `id_capture_implicit` is enabled and the SQL is a bare INSERT, call `inject_returning()` and if it returns Some, modify the SQL sent to the server AND stored in the profile.

For `is_currval_or_lastval()` queries, push `true` onto the returning queue so the DataRow response is captured.

- [ ] **Step 5: Run all tests**

Run: `cargo test 2>&1 | tail -5`
Expected: All pass.

- [ ] **Step 6: Commit**

```bash
git add src/correlate/capture.rs src/cli.rs src/proxy/connection.rs
git commit -m "feat(correlate): add --id-capture-implicit with auto-inject RETURNING and currval/lastval interception"
```

---

### Task 13: Phase 2 Integration Tests

**Files:**
- Create: `tests/id_correlate_test.rs`

- [ ] **Step 1: Write integration tests**

These tests require a running PostgreSQL instance. Follow the pattern from existing `tests/replay_e2e_test.rs` for how to conditionally skip when PG is not available.

Write tests for each ID type (serial, identity, uuid, composite key, FK chain, cross-session). Each test:
1. Creates a test table
2. Builds a workload profile programmatically with captured RETURNING values
3. Advances sequences on the target to simulate divergence
4. Replays with `--id-mode=correlate` or `--id-mode=full`
5. Asserts zero errors

See spec for the full list of per-ID-type tests.

- [ ] **Step 2: Run integration tests**

Run: `cargo test --test id_correlate_test 2>&1 | tail -20`

- [ ] **Step 3: Commit**

```bash
git add tests/id_correlate_test.rs
git commit -m "test: add Phase 2 integration tests for ID correlation (serial, identity, uuid, FK, cross-session)"
```

---

### Task 14: Criterion Benchmarks

**Files:**
- Create: `benches/substitute_bench.rs`

- [ ] **Step 1: Write benchmarks**

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use dashmap::DashMap;
use pg_retest::correlate::substitute::substitute_ids;

fn bench_substitute_no_map(c: &mut Criterion) {
    let map = DashMap::new();
    let sql = "SELECT * FROM orders WHERE id = 42 AND customer_id = 99 AND status = 'active'";
    c.bench_function("substitute_no_map", |b| {
        b.iter(|| substitute_ids(black_box(sql), &map))
    });
}

fn bench_substitute_small_map(c: &mut Criterion) {
    let map = DashMap::new();
    for i in 0..10 {
        map.insert(format!("{}", 40 + i), format!("{}", 1000 + i));
    }
    let sql = "SELECT * FROM orders WHERE id = 42 AND customer_id = 43 AND status = 'active'";
    c.bench_function("substitute_small_map", |b| {
        b.iter(|| substitute_ids(black_box(sql), &map))
    });
}

fn bench_substitute_large_map(c: &mut Criterion) {
    let map = DashMap::new();
    for i in 0..10_000 {
        map.insert(format!("{}", i), format!("{}", i + 100_000));
    }
    let sql = "SELECT * FROM orders WHERE id = 42 AND customer_id = 9999 AND status = 'active'";
    c.bench_function("substitute_large_map", |b| {
        b.iter(|| substitute_ids(black_box(sql), &map))
    });
}

fn bench_substitute_complex_query(c: &mut Criterion) {
    let map = DashMap::new();
    for i in 0..100 {
        map.insert(format!("{}", i), format!("{}", i + 100_000));
    }
    let sql = "WITH cte AS (SELECT id, name FROM customers WHERE region_id = 42 AND tier = 3) \
               SELECT o.id, o.total, c.name FROM orders o \
               JOIN cte c ON c.id = o.customer_id \
               WHERE o.status_id = 7 AND o.amount > 50 \
               ORDER BY o.created_at DESC LIMIT 100 OFFSET 20";
    c.bench_function("substitute_complex_query", |b| {
        b.iter(|| substitute_ids(black_box(sql), &map))
    });
}

fn bench_substitute_uuid_heavy(c: &mut Criterion) {
    let map = DashMap::new();
    for i in 0..1000 {
        map.insert(
            format!("{:08x}-0000-0000-0000-{:012x}", i, i),
            format!("{:08x}-ffff-ffff-ffff-{:012x}", i, i),
        );
    }
    let sql = "SELECT * FROM t WHERE \
               a = '00000005-0000-0000-0000-000000000005' AND \
               b = '00000010-0000-0000-0000-000000000010' AND \
               c = '00000050-0000-0000-0000-000000000050' AND \
               d = '00000100-0000-0000-0000-000000000100' AND \
               e = '00000500-0000-0000-0000-000000000500'";
    c.bench_function("substitute_uuid_heavy", |b| {
        b.iter(|| substitute_ids(black_box(sql), &map))
    });
}

criterion_group!(
    benches,
    bench_substitute_no_map,
    bench_substitute_small_map,
    bench_substitute_large_map,
    bench_substitute_complex_query,
    bench_substitute_uuid_heavy,
);
criterion_main!(benches);
```

- [ ] **Step 2: Run benchmarks**

Run: `cargo bench --bench substitute_bench 2>&1 | tail -30`
Expected: All benchmarks run and report timing.

- [ ] **Step 3: Commit**

```bash
git add benches/substitute_bench.rs
git commit -m "bench: add criterion benchmarks for ID substitution state machine"
```

---

### Task 15: Phase 2 Documentation

**Files:**
- Modify: `docs/id-correlation.md`
- Modify: `src/cli.rs` (help text)

- [ ] **Step 1: Update docs with correlate and full mode documentation**

Add sections for: correlate mode usage, full mode usage, `--id-capture-implicit`, `--id-correlate-all-columns`, all known limitations from the spec.

- [ ] **Step 2: Run `cargo clippy` and `cargo fmt`**

Run: `cargo fmt && cargo clippy 2>&1 | tail -20`
Expected: No warnings.

- [ ] **Step 3: Final full test run**

Run: `cargo test 2>&1 | tail -10`
Expected: All tests pass.

- [ ] **Step 4: Commit**

```bash
git add docs/id-correlation.md src/cli.rs
git commit -m "docs: complete ID correlation documentation for all modes and limitations"
```
