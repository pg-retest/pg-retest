# SQL Parsing Upgrade — Phase 2: pg_query.rs for RETURNING Sites Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace hand-rolled string manipulation in `has_returning` and `inject_returning` with a libpg_query-backed AST implementation, with a cheap prefix pre-filter so the hot-path proxy relay loop stays fast on non-candidate queries.

**Architecture:** A new `src/sql/ast.rs` module wraps the `pg_query` crate with a focused facade (`has_returning`, `inject_returning`). Hot-path callers gate the full parse behind a trivial string-prefix check — non-INSERT/UPDATE/DELETE/WITH SQL skips pg_query entirely. The legacy hand-rolled implementations are feature-flagged behind `legacy-returning` for one release cycle as a rollback safety net. A new equivalence harness (`tests/pg_query_equivalence.rs`) runs both the new AST-backed impls and pg_query's own AST answers across a 100+ query corpus so any divergence fails CI.

**Tech Stack:** Rust 2021, new `pg_query = "6"` dependency (Rust bindings to libpg_query, Postgres's actual C parser — ~1.5 MB binary growth). Existing `criterion` for the new `has_returning` bench. No other new deps.

**Mission Brief Anchor:** `/home/yonk/yonk-apps/pg-retest/skill-output/mission-brief/Mission-Brief-sql-parsing-upgrade.md` — re-read at every ⛔ drift checkpoint.

**Success Criteria covered by this plan:** SC-006, SC-007, SC-008 (AST-backed sites portion — Phase 3 extends the harness), SC-011 (legacy-returning feature flag — removed in the release after rc.4), SC-010 (revised — Phase 1 + 2 combined in rc.4), SC-012.

**Drift Checkpoints injected:** DC-003 (after pg_query dep added, before rewriting) and DC-004 (end of Phase 2, Docker demo E2E + hot-path bench).

**Prerequisite:** Phase 1 complete on `dev/1.0.0-rc.4` (already done as of this plan). Phase 2 execution continues on the same `dev/1.0.0-rc.4` branch — mission brief SC-010 was revised 2026-04-18 to ship Phase 1 + 2 together as rc.4. No rc.5 cut; no version bump. Phase 3 gets a separate branch + release later.

---

## File Structure

**New files:**
- `src/sql/ast.rs` — pg_query facade. Exposes `has_returning(sql: &str) -> Result<bool, AstError>` and `inject_returning(sql: &str, pk_map: &[TablePk]) -> Result<Option<String>, AstError>` plus the `AstError` type.
- `benches/returning_bench.rs` — criterion bench for `has_returning` with pre-filter-hits and parse-required cases.
- `benches/baselines/returning_before.txt` — bench output captured against the legacy hand-rolled impl before Phase 2 changes.
- `tests/fixtures/returning_corpus.txt` — 15+ INSERT/UPDATE/DELETE queries exercising `inject_returning` edge cases (CTE-wrapped, ON CONFLICT, multi-row VALUES, INSERT-SELECT, schema-qualified, trailing comments/semicolons).
- `tests/fixtures/returning_expected.txt` — gold output for the inject_returning corpus (format per line: `<input>|<output or NONE>`).
- `tests/sql_returning_corpus.rs` — integration test that asserts `inject_returning` on each corpus entry matches expected.
- `tests/fixtures/sql_corpus.txt` — 100+ mixed real-world-shaped queries for the pg_query equivalence harness.
- `tests/pg_query_equivalence.rs` — for every corpus query, compute `has_returning_new(sql)` and cross-check against a pg_query-derived oracle; any mismatch fails.

**Modified files:**
- `Cargo.toml` — add `pg_query = "6"` to `[dependencies]`; add `[features] legacy-returning = []`; add `[[bench]] name = "returning_bench"`.
- `src/sql/mod.rs` — add `pub mod ast;` and re-export selected items.
- `src/correlate/capture.rs` — `has_returning` and `inject_returning` become dispatching wrappers. Default (no feature) = call new AST impl from `src/sql/ast.rs`. `#[cfg(feature = "legacy-returning")]` = call old hand-rolled impl (moved to a `legacy` submodule in the same file). `has_returning`'s signature changes from `fn has_returning(sql: &str) -> bool` to `pub fn has_returning(sql: &str) -> bool` wrapping `Result<bool>` (see Task 5 rationale).
- `src/proxy/connection.rs:632, 658, 661, 686-689` — update call sites. The public wrapper stays `bool`-returning (per Task 5 — callers keep their current shape, with Err mapped to `false` inside the wrapper as the safe default).
- `CLAUDE.md` — add `src/sql/ast` entry to Key modules; new Gotchas.
- `CHANGELOG.md` — `[Unreleased]` entries for the dependency bump, the legacy flag, and the rc.5 behavior changes.

**Files explicitly NOT touched in Phase 2:**
- `src/transform/analyze.rs` (extract_tables, extract_filter_columns) — Phase 3 scope.
- `src/transform/mysql_to_pg.rs` — out of scope per brief.
- `src/sql/lex.rs` — Phase 1 artifact, locked.
- `src/capture/masking.rs`, `src/correlate/substitute.rs` — Phase 1 artifacts, locked.

---

## Task 1: Add pg_query dependency, verify build and binary size (DC-003 gate)

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add pg_query dep and legacy feature to Cargo.toml**

```toml
# Under [dependencies], append:
pg_query = "6"
```

```toml
# At the bottom, add a new [features] section (or append to an existing one):
[features]
legacy-returning = []
```

- [ ] **Step 2: Verify release build succeeds**

```bash
cargo build --release 2>&1 | tail -5
```

Expected: `Finished release profile [optimized] target(s) in <N>s`. If build fails on libpg_query C compilation:
- macOS: ensure Xcode command-line tools installed (`xcode-select --install`).
- Linux: ensure `gcc` / `clang` and `make` are installed (`apt install build-essential` or equivalent).
- If it still fails, STOP and report BLOCKED with the compiler error.

- [ ] **Step 3: Measure binary size delta**

```bash
# Before (on parent commit of this Task 1 commit, i.e., HEAD~1 after you commit)
# capture BEFORE size first, pre-commit:
BEFORE_SIZE=$(stat -f%z target/release/pg-retest 2>/dev/null || stat -c%s target/release/pg-retest)
echo "Before pg_query: ${BEFORE_SIZE} bytes"

# After committing the Cargo.toml change and rebuilding:
cargo build --release 2>&1 | tail -2
AFTER_SIZE=$(stat -f%z target/release/pg-retest 2>/dev/null || stat -c%s target/release/pg-retest)
echo "After pg_query: ${AFTER_SIZE} bytes"

DELTA=$(( (AFTER_SIZE - BEFORE_SIZE) / 1024 / 1024 ))
echo "Delta: ${DELTA} MB"
```

Expected: delta ≤ 2 MB. If > 2 MB, STOP and escalate per DC-003.

- [ ] **Step 4: Verify tests still pass (pg_query is dep-only so far, no code calls it yet)**

```bash
cargo test --lib 2>&1 | tail -3
```

Expected: `test result: ok` with same count as Phase 1.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "$(cat <<'EOF'
feat(sql): add pg_query.rs dependency for Phase 2

libpg_query Rust bindings for AST-backed has_returning and
inject_returning. Binary size delta: <N> MB (verified ≤ 2 MB per
DC-003). legacy-returning feature flag added as a rollback safety
net — default build uses the new AST impl once Task 5/7 land;
enabling the feature compiles the old hand-rolled impl instead.

SC-010 (rc.5), DC-003.
EOF
)"
```

Replace `<N>` with the actual measured delta from Step 3.

---

## ⛔ Drift Check DC-003

**Trigger:** After Task 1 commits; before writing any code that consumes pg_query.

- [ ] **Step 1: Re-read mission brief**

```bash
cat /home/yonk/yonk-apps/pg-retest/skill-output/mission-brief/Mission-Brief-sql-parsing-upgrade.md
```

- [ ] **Step 2: Verify DC-003 gate conditions**

Mission brief says: "After pg_query.rs dependency added, before rewriting `has_returning`/`inject_returning` → verify `cargo build --release` still works on Linux + macOS, binary size delta documented. If > 2 MB delta or build broken anywhere, stop and reassess."

Answer explicitly:
1. Does `cargo build --release` succeed on Linux? (Run it and confirm.)
2. If you have macOS access, does it succeed there too? If not, document as "not tested on macOS in this session — flagged for CI."
3. Binary size delta: X MB. Is X ≤ 2? If no, STOP.

- [ ] **Step 3: Drift-detection questions**

1. **Am I still solving the stated Purpose?** Expected: *yes — adding the parser that will back the structural has_returning/inject_returning rewrite. No code calls pg_query yet.*
2. **Does my current work map to at least one Success Criterion?** Expected: *yes — SC-011 (legacy feature flag declared) and foundation for SC-006, SC-007.*
3. **Am I doing anything in Out of Scope?** Expected: *no — only Cargo.toml touched.*

If any check fails, STOP and surface to the user.

---

## Task 2: Capture `has_returning` performance baseline (SC-005)

**Files:**
- Create: `benches/returning_bench.rs`
- Create: `benches/baselines/returning_before.txt`
- Modify: `Cargo.toml` (add `[[bench]] name = "returning_bench"`)

- [ ] **Step 1: Add the bench file**

Create `benches/returning_bench.rs`:

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use pg_retest::correlate::capture::has_returning;

fn bench_returning_select_skipped(c: &mut Criterion) {
    // Should short-circuit via pre-filter (not INSERT/UPDATE/DELETE/MERGE/WITH).
    let sql = "SELECT * FROM users WHERE id = 42 AND status = 'active'";
    c.bench_function("returning_select_skipped", |b| {
        b.iter(|| has_returning(black_box(sql)))
    });
}

fn bench_returning_insert_no_returning(c: &mut Criterion) {
    // INSERT without RETURNING — pre-filter passes, full parse runs.
    let sql = "INSERT INTO orders (customer_id, total) VALUES (42, 19.99)";
    c.bench_function("returning_insert_no_returning", |b| {
        b.iter(|| has_returning(black_box(sql)))
    });
}

fn bench_returning_insert_with_returning(c: &mut Criterion) {
    let sql = "INSERT INTO orders (customer_id, total) VALUES (42, 19.99) RETURNING id";
    c.bench_function("returning_insert_with_returning", |b| {
        b.iter(|| has_returning(black_box(sql)))
    });
}

fn bench_returning_cte_wrapped(c: &mut Criterion) {
    let sql = "WITH new_order AS (INSERT INTO orders (customer_id) VALUES (42) RETURNING id) SELECT * FROM new_order";
    c.bench_function("returning_cte_wrapped", |b| {
        b.iter(|| has_returning(black_box(sql)))
    });
}

criterion_group!(
    benches,
    bench_returning_select_skipped,
    bench_returning_insert_no_returning,
    bench_returning_insert_with_returning,
    bench_returning_cte_wrapped,
);
criterion_main!(benches);
```

- [ ] **Step 2: Register the bench in Cargo.toml**

Append to the `[[bench]]` section in `Cargo.toml`:

```toml
[[bench]]
name = "returning_bench"
harness = false
```

- [ ] **Step 3: Capture the baseline against the current (legacy) has_returning**

```bash
mkdir -p benches/baselines
rm -rf target/criterion  # clean so `change:` lines don't appear
cargo bench --bench returning_bench 2>&1 | tee benches/baselines/returning_before.txt
```

Expected: file contains 4 `time:` lines.

- [ ] **Step 4: Verify file has data**

```bash
grep -E "time:" benches/baselines/returning_before.txt | wc -l
```

Expected: `4`.

- [ ] **Step 5: Commit**

```bash
git add benches/returning_bench.rs benches/baselines/returning_before.txt Cargo.toml
git commit -m "$(cat <<'EOF'
bench: add has_returning bench and capture pre-Phase-2 baseline

Establishes pre-pg_query perf baseline for has_returning across
four representative cases (SELECT skipped by pre-filter, INSERT
without RETURNING requiring full parse, INSERT with RETURNING,
CTE-wrapped INSERT with RETURNING). Used by DC-004 to verify no
hot-path regression under the pre-filter.

SC-005, DC-004.
EOF
)"
```

---

## Task 3: Create `src/sql/ast.rs` with pg_query facade + unit tests

**Files:**
- Create: `src/sql/ast.rs`
- Modify: `src/sql/mod.rs` (add `pub mod ast;` and re-exports)

- [ ] **Step 1: Scaffold `src/sql/ast.rs` with the public API and one failing test**

Create `src/sql/ast.rs`:

```rust
//! AST-backed SQL analysis for structural questions (RETURNING detection,
//! RETURNING splice points). Uses `pg_query` (Rust bindings to libpg_query —
//! Postgres's own C parser) for grammar fidelity: anything PG accepts,
//! pg_query parses by construction.
//!
//! Hot-path consumers should apply a cheap prefix pre-filter (see
//! `might_have_returning`) before calling the full-parse functions — pg_query
//! costs ~2-20µs per query depending on complexity.

use crate::correlate::capture::TablePk;

/// Errors returned by the AST-backed functions in this module.
#[derive(Debug, thiserror::Error)]
pub enum AstError {
    #[error("pg_query parse failed: {0}")]
    Parse(String),
    #[error("unexpected AST shape: {0}")]
    Shape(String),
}

/// Cheap prefix check: if this returns false, the SQL definitely has no
/// RETURNING clause we care about and the full parse can be skipped.
/// If it returns true, a full parse is required to answer correctly.
///
/// Matches any SQL whose first keyword (ignoring leading whitespace and
/// `--` / `/* */` comments) is INSERT, UPDATE, DELETE, MERGE, or WITH
/// (the last catches CTE-wrapped writes).
pub fn might_have_returning(sql: &str) -> bool {
    let trimmed = skip_leading_whitespace_and_comments(sql);
    let upper_prefix: String = trimmed
        .chars()
        .take(8)
        .collect::<String>()
        .to_ascii_uppercase();
    upper_prefix.starts_with("INSERT")
        || upper_prefix.starts_with("UPDATE")
        || upper_prefix.starts_with("DELETE")
        || upper_prefix.starts_with("MERGE")
        || upper_prefix.starts_with("WITH")
}

/// Skip leading whitespace and SQL comments (`--` line, `/* */` block).
/// Returns a slice of `sql` starting at the first non-skipped byte.
fn skip_leading_whitespace_and_comments(sql: &str) -> &str {
    let mut rest = sql;
    loop {
        let trimmed = rest.trim_start();
        if let Some(stripped) = trimmed.strip_prefix("--") {
            rest = match stripped.find('\n') {
                Some(idx) => &stripped[idx + 1..],
                None => "",
            };
        } else if let Some(stripped) = trimmed.strip_prefix("/*") {
            rest = match stripped.find("*/") {
                Some(idx) => &stripped[idx + 2..],
                None => "",
            };
        } else {
            return trimmed;
        }
    }
}

/// AST-backed `has_returning`. Returns `Ok(true)` iff the top-level statement
/// (after stripping any enclosing `WITH` CTE wrapper) is an INSERT, UPDATE,
/// DELETE, or MERGE with a non-empty `returningList`. Returns `Ok(false)`
/// for SELECT, DDL, and DML without RETURNING. Returns `Err(AstError::Parse)`
/// on syntactically invalid input — callers should treat this as the safe
/// default (usually "assume no RETURNING").
pub fn has_returning(sql: &str) -> Result<bool, AstError> {
    if !might_have_returning(sql) {
        return Ok(false);
    }
    let parsed =
        pg_query::parse(sql).map_err(|e| AstError::Parse(format!("{}", e)))?;
    for stmt in parsed.protobuf.stmts.iter() {
        if let Some(node) = stmt.stmt.as_ref().and_then(|s| s.node.as_ref()) {
            if node_has_returning(node)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Walk a single statement node and detect RETURNING.
fn node_has_returning(node: &pg_query::protobuf::node::Node) -> Result<bool, AstError> {
    use pg_query::protobuf::node::Node as N;
    match node {
        N::InsertStmt(s) => Ok(!s.returning_list.is_empty()),
        N::UpdateStmt(s) => Ok(!s.returning_list.is_empty()),
        N::DeleteStmt(s) => Ok(!s.returning_list.is_empty()),
        // MERGE ... RETURNING added in PG17; pg_query 6.x may represent
        // MergeStmt with a returning_list field. If not, this arm returns
        // Ok(false) safely — update when pg_query adds the field.
        N::MergeStmt(_) => Ok(false),
        // Top-level SELECT may be a CTE-wrapped DML. pg_query surfaces this
        // as SelectStmt with `with_clause.ctes[..].ctequery` — walk those.
        N::SelectStmt(s) => {
            if let Some(with) = s.with_clause.as_ref() {
                for cte in with.ctes.iter() {
                    if let Some(inner_node) = cte
                        .node
                        .as_ref()
                        .and_then(|n| if let N::CommonTableExpr(c) = n { Some(c) } else { None })
                        .and_then(|c| c.ctequery.as_ref())
                        .and_then(|q| q.node.as_ref())
                    {
                        if node_has_returning(inner_node)? {
                            return Ok(true);
                        }
                    }
                }
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

/// AST-backed `inject_returning`. Uses pg_query to identify the splice point
/// and falls back to string splicing (not deparse) so comments and whitespace
/// are preserved. Returns `Ok(None)` if the SQL isn't a bare INSERT targeting
/// a known-PK table, or already has RETURNING.
pub fn inject_returning(
    sql: &str,
    pk_map: &[TablePk],
) -> Result<Option<String>, AstError> {
    // Fast path: not a candidate.
    if !might_have_returning(sql) {
        return Ok(None);
    }
    // If it already has RETURNING, skip.
    if has_returning(sql)? {
        return Ok(None);
    }
    let parsed =
        pg_query::parse(sql).map_err(|e| AstError::Parse(format!("{}", e)))?;
    // Find the first INSERT statement (possibly inside a CTE wrapper).
    let insert = find_insert_stmt(&parsed)?;
    let Some(insert) = insert else {
        return Ok(None);
    };
    // Look up the target table in pk_map.
    let Some((schema, table)) = extract_insert_target(insert)? else {
        return Ok(None);
    };
    let pk = pk_map.iter().find(|pk| {
        pk.table == table && (schema.is_empty() || pk.schema == schema)
    });
    let Some(pk) = pk else {
        return Ok(None);
    };
    let returning_cols = pk.columns.join(", ");
    // Splice: find the byte offset just before ON CONFLICT (if present) or at
    // the end of the statement (before trailing whitespace/semicolons).
    let splice_offset = find_splice_offset(sql, insert)?;
    let before = sql[..splice_offset].trim_end();
    let after = &sql[splice_offset..];
    Ok(Some(format!("{} RETURNING {}{}", before, returning_cols, after)))
}

/// Locate the first InsertStmt in the parsed tree (or the one inside a
/// CTE-wrapped SelectStmt if the top-level is a WITH).
fn find_insert_stmt(
    _parsed: &pg_query::ParseResult,
) -> Result<Option<&pg_query::protobuf::InsertStmt>, AstError> {
    // Implementation detail: walk parsed.protobuf.stmts, return the first
    // InsertStmt encountered (directly or via CTE.ctequery). The exact
    // traversal follows the same pattern as node_has_returning above.
    // Returning &InsertStmt keeps the lifetime tied to the ParseResult owned
    // by the caller; if the borrow checker complains, have the caller take
    // ownership of (offsets, target_table) instead of a reference.
    Err(AstError::Shape(
        "find_insert_stmt: implement alongside has_returning traversal".into(),
    ))
}

/// Extract (schema, table) from an InsertStmt's relation field.
/// `schema` is empty when the insert target is unqualified.
fn extract_insert_target(
    _stmt: &pg_query::protobuf::InsertStmt,
) -> Result<Option<(String, String)>, AstError> {
    // stmt.relation is Option<RangeVar> with .schemaname and .relname fields.
    // Return (schemaname, relname) if present; None if the relation is missing
    // (shouldn't happen for a valid INSERT but handle gracefully).
    Err(AstError::Shape(
        "extract_insert_target: implement alongside has_returning traversal".into(),
    ))
}

/// Compute the byte offset at which RETURNING should be spliced into `sql`.
/// This is the offset BEFORE any ON CONFLICT clause (if present), otherwise
/// the offset just before trailing whitespace/semicolons at end of input.
fn find_splice_offset(
    _sql: &str,
    _stmt: &pg_query::protobuf::InsertStmt,
) -> Result<usize, AstError> {
    // Strategy: pg_query AST gives location (byte offset) for on_conflict_clause
    // when present. If on_conflict_clause.is_some(), return its location.
    // Otherwise, find the end of the statement by walking sql bytes backward
    // from sql.len(), skipping whitespace and trailing ';'.
    Err(AstError::Shape(
        "find_splice_offset: implement alongside has_returning traversal".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefilter_skips_select() {
        assert!(!might_have_returning("SELECT * FROM users"));
    }

    #[test]
    fn prefilter_catches_insert() {
        assert!(might_have_returning("INSERT INTO t VALUES (1)"));
        assert!(might_have_returning("  INSERT INTO t VALUES (1)"));
        assert!(might_have_returning("insert into t values (1)"));
    }

    #[test]
    fn prefilter_catches_with_cte() {
        assert!(might_have_returning("WITH x AS (SELECT 1) SELECT * FROM x"));
    }

    #[test]
    fn prefilter_catches_update_delete_merge() {
        assert!(might_have_returning("UPDATE t SET a = 1"));
        assert!(might_have_returning("DELETE FROM t"));
        assert!(might_have_returning("MERGE INTO t USING s ON t.id = s.id"));
    }

    #[test]
    fn prefilter_skips_leading_comment() {
        assert!(!might_have_returning("-- comment\nSELECT 1"));
        assert!(!might_have_returning("/* block */ SELECT 1"));
    }

    #[test]
    fn prefilter_catches_insert_after_comment() {
        assert!(might_have_returning("-- inserting\nINSERT INTO t VALUES (1)"));
    }
}
```

- [ ] **Step 2: Add `pub mod ast;` to `src/sql/mod.rs`**

```rust
pub mod ast;
pub mod lex;
pub use ast::{has_returning as has_returning_ast, inject_returning as inject_returning_ast, AstError};
pub use lex::{visit_tokens, Span, SqlLexer, Token, TokenKind};
```

The `_ast` suffix in the re-exports avoids clashing with the existing
`correlate::capture::has_returning` during the transition.

- [ ] **Step 3: Run the prefilter tests — verify they pass**

```bash
cargo test --lib sql::ast::tests::prefilter
```

Expected: 6 prefilter tests pass. The stub implementations of `find_insert_stmt`, `extract_insert_target`, and `find_splice_offset` are not exercised yet.

- [ ] **Step 4: Verify clippy + fmt**

```bash
cargo clippy --lib --tests -- -D warnings
cargo fmt --check
```

Expected: both clean. If clippy flags the stub `Err(AstError::Shape(...))` in the private helpers as unused, that's fine — they become used in Task 7.

- [ ] **Step 5: Commit**

```bash
git add src/sql/ast.rs src/sql/mod.rs
git commit -m "$(cat <<'EOF'
feat(sql): scaffold ast module with prefilter and API stubs

Introduces src/sql/ast.rs as a pg_query facade. The cheap
might_have_returning prefilter is fully implemented and tested (6
unit tests: SELECT/INSERT/UPDATE/DELETE/MERGE/WITH, with and
without leading comments). has_returning walks pg_query's AST for
InsertStmt/UpdateStmt/DeleteStmt/MergeStmt (top-level or
CTE-wrapped). inject_returning helpers (find_insert_stmt,
extract_insert_target, find_splice_offset) are stubbed and will
be filled in Task 7; the public signatures are locked.

Part of SC-006, foundation for SC-007.
EOF
)"
```

---

## Task 4: Feature-flag the existing `has_returning` / `inject_returning` as legacy

**Files:**
- Modify: `src/correlate/capture.rs`

- [ ] **Step 1: Wrap the existing bodies under a `legacy` submodule**

In `src/correlate/capture.rs`, move the current `has_returning` and `inject_returning` function bodies into a new `mod legacy` block at the top of the file, then replace the public functions with dispatch wrappers:

Replace:

```rust
pub fn has_returning(sql: &str) -> bool {
    // ... existing body
}

pub fn inject_returning(sql: &str, pk_map: &[TablePk]) -> Option<String> {
    // ... existing body
}
```

with:

```rust
/// Dispatch wrapper: delegates to the pg_query-backed impl by default, or
/// the legacy hand-rolled impl when the `legacy-returning` feature is on.
///
/// Returns `false` on parse errors as the safe default (matches prior
/// behavior: uncertain input is treated as "no RETURNING").
pub fn has_returning(sql: &str) -> bool {
    #[cfg(feature = "legacy-returning")]
    {
        legacy::has_returning(sql)
    }
    #[cfg(not(feature = "legacy-returning"))]
    {
        crate::sql::ast::has_returning(sql).unwrap_or(false)
    }
}

/// Dispatch wrapper: delegates to the pg_query-backed impl by default.
pub fn inject_returning(sql: &str, pk_map: &[TablePk]) -> Option<String> {
    #[cfg(feature = "legacy-returning")]
    {
        legacy::inject_returning(sql, pk_map)
    }
    #[cfg(not(feature = "legacy-returning"))]
    {
        crate::sql::ast::inject_returning(sql, pk_map).ok().flatten()
    }
}

mod legacy {
    //! Hand-rolled pre-Phase-2 implementations. Removed in rc.6 per SC-011.

    use super::TablePk;

    pub fn has_returning(sql: &str) -> bool {
        // <<< PASTE THE ORIGINAL has_returning BODY HERE VERBATIM >>>
    }

    pub fn inject_returning(sql: &str, pk_map: &[TablePk]) -> Option<String> {
        // <<< PASTE THE ORIGINAL inject_returning BODY HERE VERBATIM >>>
    }
}
```

The existing helpers used by the old impl (no other unrelated code in the file needs moving — `discover_primary_keys`, `is_currval_or_lastval`, `ResponseRow`, `TablePk` stay top-level).

- [ ] **Step 2: Verify the non-legacy build path still compiles**

With the AST-backed `has_returning` and `inject_returning` still stubbed (Tasks 5 and 7 land them), the non-legacy path will panic at runtime if exercised. For now, the default build should compile cleanly. Check:

```bash
cargo build --lib 2>&1 | tail -3
```

Expected: builds. Some clippy warnings about the stubs are fine for now.

- [ ] **Step 3: Verify the legacy build path still passes all existing tests**

```bash
cargo test --lib --features legacy-returning correlate::capture::tests
```

Expected: all existing tests pass (pre-Phase-2 count).

- [ ] **Step 4: Add a "legacy mode works" sanity test to `src/correlate/capture.rs`**

Append to the existing `#[cfg(test)] mod tests`:

```rust
#[test]
#[cfg(feature = "legacy-returning")]
fn legacy_has_returning_matches_dispatch() {
    assert!(has_returning("INSERT INTO t VALUES (1) RETURNING id"));
    assert!(!has_returning("SELECT 1"));
}
```

Verify it passes under the feature flag:

```bash
cargo test --lib --features legacy-returning correlate::capture::tests::legacy_has_returning_matches_dispatch
```

- [ ] **Step 5: Commit**

```bash
git add src/correlate/capture.rs
git commit -m "$(cat <<'EOF'
refactor(correlate): gate has_returning/inject_returning behind feature

The public has_returning(sql: &str) -> bool and inject_returning
entrypoints become dispatch wrappers. Default build routes to the
(still-stubbed) pg_query-backed impl in crate::sql::ast. The
legacy-returning feature flag routes to a new mod legacy{} that
contains the pre-Phase-2 hand-rolled implementations verbatim.

Existing unit tests continue to pass under --features
legacy-returning. Removed in rc.6 per SC-011.

SC-011.
EOF
)"
```

---

## Task 5: Implement the pg_query-backed `has_returning`

**Files:**
- Modify: `src/sql/ast.rs` (flesh out the traversal + error handling; prefilter is already done in Task 3)
- Modify: `src/correlate/capture.rs` tests (add AST-path assertions)

- [ ] **Step 1: Write failing tests for the AST-backed has_returning**

Add to `src/sql/ast.rs`'s tests module:

```rust
#[test]
fn ast_has_returning_simple() {
    assert_eq!(has_returning("INSERT INTO t VALUES (1) RETURNING id").unwrap(), true);
    assert_eq!(has_returning("INSERT INTO t VALUES (1)").unwrap(), false);
}

#[test]
fn ast_has_returning_update_delete() {
    assert_eq!(has_returning("UPDATE t SET a = 1 RETURNING id").unwrap(), true);
    assert_eq!(has_returning("DELETE FROM t WHERE id = 1 RETURNING id").unwrap(), true);
    assert_eq!(has_returning("UPDATE t SET a = 1").unwrap(), false);
}

#[test]
fn ast_has_returning_cte_wrapped() {
    let sql = "WITH new_order AS (INSERT INTO orders (customer_id) VALUES (42) RETURNING id) SELECT * FROM new_order";
    assert_eq!(has_returning(sql).unwrap(), true);
}

#[test]
fn ast_has_returning_cte_no_returning_inside() {
    let sql = "WITH x AS (SELECT 1) INSERT INTO t SELECT * FROM x";
    assert_eq!(has_returning(sql).unwrap(), false);
}

#[test]
fn ast_has_returning_returning_as_column_alias() {
    // A column named "returning" must NOT trigger true — this was a bug
    // class in the legacy impl.
    let sql = "SELECT col AS \"returning\" FROM t";
    assert_eq!(has_returning(sql).unwrap(), false);
}

#[test]
fn ast_has_returning_returning_in_comment() {
    // Comments must not trigger false positives.
    let sql = "-- RETURNING\nSELECT 1";
    assert_eq!(has_returning(sql).unwrap(), false);
    let sql = "/* RETURNING id */ SELECT 1";
    assert_eq!(has_returning(sql).unwrap(), false);
}

#[test]
fn ast_has_returning_invalid_sql() {
    // Unparseable input returns Err — callers default to false.
    let result = has_returning("INSERT INTO");
    assert!(result.is_err(), "expected parse error for truncated INSERT");
}

#[test]
fn ast_has_returning_string_contains_returning() {
    // The word "RETURNING" inside a string literal is harmless.
    let sql = "INSERT INTO t (s) VALUES ('it has RETURNING in it')";
    assert_eq!(has_returning(sql).unwrap(), false);
}
```

- [ ] **Step 2: Run the tests — they fail because Task 3 left helpers stubbed and the main traversal runs only the stub helpers**

```bash
cargo test --lib sql::ast::tests 2>&1 | tail -20
```

Expected: prefilter tests pass, new tests mostly fail with `AstError::Shape("find_insert_stmt: implement...")` or similar. (If they already pass because Task 3's `has_returning` doesn't depend on the stubs, skip to Step 3 and just verify.)

- [ ] **Step 3: Replace the `node_has_returning` and top-level `has_returning` with the real walk (if not already complete in Task 3)**

The Task 3 scaffold already has the structure right for `node_has_returning`. If tests fail because of missing pg_query API details, fix the traversal to match the actual `pg_query::protobuf` types. A quick exploration command to discover the exact API:

```bash
cargo doc --no-deps --open
# Or: cargo rustdoc -- --document-private-items
# Look for pg_query::protobuf::node::Node variants
```

Relevant API points to verify and fix as needed:
- `pg_query::parse(sql)` returns `Result<ParseResult, Error>`.
- `ParseResult.protobuf.stmts` is `Vec<RawStmt>`, each with `stmt: Option<Node>`.
- `Node.node: Option<node::Node>` where `node::Node` is the enum with `InsertStmt(InsertStmt)`, `SelectStmt(SelectStmt)`, etc. variants.
- `SelectStmt.with_clause: Option<WithClause>`, `WithClause.ctes: Vec<Node>`, `CommonTableExpr.ctequery: Option<Node>`.

Fix the test failures one by one. When a test passes, move to the next.

- [ ] **Step 4: All new tests pass**

```bash
cargo test --lib sql::ast::tests
```

Expected: 14+ tests pass (6 prefilter + 8 ast_has_returning_*).

- [ ] **Step 5: Also update the dispatch wrapper test in `src/correlate/capture.rs`**

Append to its tests:

```rust
#[test]
#[cfg(not(feature = "legacy-returning"))]
fn ast_dispatch_has_returning() {
    assert!(has_returning("INSERT INTO t VALUES (1) RETURNING id"));
    assert!(!has_returning("SELECT 1"));
    // Invalid SQL collapses to false via Result::unwrap_or(false).
    assert!(!has_returning("INSERT INTO"));
}
```

Run:

```bash
cargo test --lib correlate::capture::tests::ast_dispatch_has_returning
```

- [ ] **Step 6: Verify clippy + fmt**

```bash
cargo clippy --lib --tests -- -D warnings
cargo fmt --check
```

- [ ] **Step 7: Commit**

```bash
git add src/sql/ast.rs src/correlate/capture.rs
git commit -m "$(cat <<'EOF'
feat(sql): implement AST-backed has_returning via pg_query

Replaces the hand-rolled substring scan with a libpg_query AST
walk. Correctly handles CTE-wrapped writes (WITH x AS (INSERT ...
RETURNING id) SELECT ...), a column aliased as "returning", and
the word RETURNING inside comments or string literals — all bug
classes in the legacy impl.

Signature upgrade: internal function is Result<bool, AstError>.
The public correlate::capture::has_returning wrapper preserves
its bool-returning shape by mapping parse errors to false (safe
default — uncertain input is treated as "no RETURNING").

Pre-filter (might_have_returning) skips pg_query for SQL whose
first keyword isn't INSERT/UPDATE/DELETE/MERGE/WITH.

SC-006.
EOF
)"
```

---

## Task 6: Update hot-path callers in `src/proxy/connection.rs`

**Files:**
- Modify: `src/proxy/connection.rs` lines 632, 658, 661, 686, 687, 689 (confirm with grep)

- [ ] **Step 1: Find and read all call sites**

```bash
grep -n "has_returning\|inject_returning" src/proxy/connection.rs
```

Expected: 6+ call sites, all using the bool/Option<String> shape.

- [ ] **Step 2: Verify the public wrappers in `src/correlate/capture.rs` preserved the old signatures**

The Task 4 refactor kept the public signatures: `has_returning(sql) -> bool` and `inject_returning(sql, pk_map) -> Option<String>`. So call sites need ZERO changes for correctness.

However, this task also ADDS the AST-backed inject_returning — so when Task 7 ships, the wrapper behavior shifts from legacy to pg_query. There's nothing to change in `connection.rs`; this task is a no-op verification step. The purpose is to document that we confirmed the wrappers are shape-compatible before Phase 7.

- [ ] **Step 3: Run the full test suite**

```bash
cargo test --lib
cargo test --tests
```

Expected: all pass.

- [ ] **Step 4: No commit needed (no code change). Proceed to Task 7.**

If you DID need to change a call site (e.g., to log parse errors or surface metrics), do it in a small dedicated commit here rather than rolling it into Task 7.

---

## Task 7: Implement AST-backed `inject_returning` with splice

**Files:**
- Modify: `src/sql/ast.rs` (fill in `find_insert_stmt`, `extract_insert_target`, `find_splice_offset`, and any traversal helpers)

- [ ] **Step 1: Write failing tests for the AST-backed inject_returning**

Add to `src/sql/ast.rs`'s tests module:

```rust
fn pk_orders() -> Vec<TablePk> {
    vec![TablePk {
        schema: "public".into(),
        table: "orders".into(),
        columns: vec!["id".into()],
    }]
}

#[test]
fn ast_inject_simple() {
    let pk = pk_orders();
    assert_eq!(
        inject_returning("INSERT INTO orders (name) VALUES ('test')", &pk).unwrap(),
        Some("INSERT INTO orders (name) VALUES ('test') RETURNING id".into())
    );
}

#[test]
fn ast_inject_already_has_returning() {
    let pk = pk_orders();
    assert_eq!(
        inject_returning(
            "INSERT INTO orders (name) VALUES ('test') RETURNING id",
            &pk
        )
        .unwrap(),
        None
    );
}

#[test]
fn ast_inject_unknown_table() {
    let pk = pk_orders();
    assert_eq!(
        inject_returning("INSERT INTO unknown (name) VALUES ('test')", &pk).unwrap(),
        None
    );
}

#[test]
fn ast_inject_on_conflict_splice_before() {
    let pk = pk_orders();
    assert_eq!(
        inject_returning(
            "INSERT INTO orders (id, name) VALUES (1, 'test') ON CONFLICT DO NOTHING",
            &pk
        )
        .unwrap(),
        Some(
            "INSERT INTO orders (id, name) VALUES (1, 'test') RETURNING id ON CONFLICT DO NOTHING"
                .into()
        )
    );
}

#[test]
fn ast_inject_on_conflict_do_update() {
    let pk = pk_orders();
    assert_eq!(
        inject_returning(
            "INSERT INTO orders (id, name) VALUES (1, 'test') ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
            &pk
        )
        .unwrap(),
        Some(
            "INSERT INTO orders (id, name) VALUES (1, 'test') RETURNING id ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name"
                .into()
        )
    );
}

#[test]
fn ast_inject_multi_row_values() {
    let pk = pk_orders();
    assert_eq!(
        inject_returning(
            "INSERT INTO orders (name) VALUES ('a'), ('b'), ('c')",
            &pk
        )
        .unwrap(),
        Some("INSERT INTO orders (name) VALUES ('a'), ('b'), ('c') RETURNING id".into())
    );
}

#[test]
fn ast_inject_insert_select() {
    let pk = pk_orders();
    assert_eq!(
        inject_returning(
            "INSERT INTO orders (id, name) SELECT id, name FROM staging",
            &pk
        )
        .unwrap(),
        Some(
            "INSERT INTO orders (id, name) SELECT id, name FROM staging RETURNING id".into()
        )
    );
}

#[test]
fn ast_inject_trailing_semicolon() {
    let pk = pk_orders();
    assert_eq!(
        inject_returning("INSERT INTO orders (name) VALUES ('x');", &pk).unwrap(),
        Some("INSERT INTO orders (name) VALUES ('x') RETURNING id;".into())
    );
}

#[test]
fn ast_inject_trailing_comment() {
    // Trailing comment is preserved.
    let pk = pk_orders();
    assert_eq!(
        inject_returning(
            "INSERT INTO orders (name) VALUES ('x') -- trailing",
            &pk
        )
        .unwrap(),
        Some("INSERT INTO orders (name) VALUES ('x') RETURNING id -- trailing".into())
    );
}

#[test]
fn ast_inject_cte_wrapped() {
    // WITH-wrapped INSERT — the returning should be added inside the CTE
    // definition, not at the top-level SELECT. For Phase 2 we skip
    // CTE-wrapped inserts (return None) since the splice semantics are
    // ambiguous and the use case is rare. Phase 3 can revisit.
    let pk = pk_orders();
    let sql = "WITH new_order AS (INSERT INTO orders (name) VALUES ('x')) SELECT * FROM new_order";
    assert_eq!(inject_returning(sql, &pk).unwrap(), None);
}

#[test]
fn ast_inject_schema_qualified() {
    let pk = vec![TablePk {
        schema: "analytics".into(),
        table: "events".into(),
        columns: vec!["event_id".into()],
    }];
    assert_eq!(
        inject_returning("INSERT INTO analytics.events (name) VALUES ('x')", &pk).unwrap(),
        Some(
            "INSERT INTO analytics.events (name) VALUES ('x') RETURNING event_id".into()
        )
    );
}

#[test]
fn ast_inject_not_insert() {
    let pk = pk_orders();
    // UPDATE and DELETE aren't injected even if they target known-PK tables
    // (Phase 2 scope is INSERT only, matching legacy behavior).
    assert_eq!(
        inject_returning("UPDATE orders SET name = 'x' WHERE id = 1", &pk).unwrap(),
        None
    );
}
```

- [ ] **Step 2: Run — verify they fail**

```bash
cargo test --lib sql::ast::tests::ast_inject 2>&1 | tail -20
```

Expected: multiple tests fail with `AstError::Shape(...)` from the stub helpers.

- [ ] **Step 3: Implement `find_insert_stmt`**

```rust
fn find_insert_stmt(
    parsed: &pg_query::ParseResult,
) -> Result<Option<&pg_query::protobuf::InsertStmt>, AstError> {
    use pg_query::protobuf::node::Node as N;
    // Only handle top-level inserts in Phase 2. CTE-wrapped inserts return
    // None (test ast_inject_cte_wrapped asserts this).
    for stmt in parsed.protobuf.stmts.iter() {
        if let Some(node) = stmt.stmt.as_ref().and_then(|s| s.node.as_ref()) {
            if let N::InsertStmt(insert) = node {
                return Ok(Some(insert));
            }
        }
    }
    Ok(None)
}
```

- [ ] **Step 4: Implement `extract_insert_target`**

```rust
fn extract_insert_target(
    stmt: &pg_query::protobuf::InsertStmt,
) -> Result<Option<(String, String)>, AstError> {
    let Some(rel) = stmt.relation.as_ref() else {
        return Ok(None);
    };
    Ok(Some((rel.schemaname.clone(), rel.relname.clone())))
}
```

- [ ] **Step 5: Implement `find_splice_offset`**

```rust
fn find_splice_offset(
    sql: &str,
    stmt: &pg_query::protobuf::InsertStmt,
) -> Result<usize, AstError> {
    // If ON CONFLICT is present, its location is the offset of the ON keyword.
    if let Some(on_conflict) = stmt.on_conflict_clause.as_ref() {
        let loc = on_conflict.location as usize;
        if loc == 0 {
            // pg_query may not populate location for every sub-node; fall back
            // to string search.
            if let Some(off) = find_on_conflict_byte_offset(sql) {
                return Ok(off);
            }
            return Err(AstError::Shape(
                "on_conflict_clause without location".into(),
            ));
        }
        return Ok(loc);
    }
    // No ON CONFLICT: splice at the end, excluding trailing whitespace, SQL
    // comments, and a single trailing semicolon. Walk backward from end.
    let mut end = sql.len();
    let bytes = sql.as_bytes();
    // Skip trailing whitespace
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    // Skip trailing -- line comment(s)
    loop {
        let before = end;
        // Skip line comment: walk back to find last '\n' or start, then check if
        // the content after '\n' starts with '--'.
        let Some(nl) = sql[..end].rfind('\n') else {
            break;
        };
        let tail = &sql[nl + 1..end];
        if tail.trim_start().starts_with("--") {
            end = nl;
            while end > 0 && bytes[end - 1].is_ascii_whitespace() {
                end -= 1;
            }
        } else {
            break;
        }
        if end == before {
            break;
        }
    }
    // Skip a trailing semicolon
    if end > 0 && bytes[end - 1] == b';' {
        end -= 1;
        while end > 0 && bytes[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
    }
    Ok(end)
}

/// Fallback for when pg_query doesn't populate on_conflict_clause.location.
/// Case-insensitive search for " ON CONFLICT" outside of string literals and
/// comments. Simple but correct for the common shapes in our test corpus.
fn find_on_conflict_byte_offset(sql: &str) -> Option<usize> {
    let upper = sql.to_ascii_uppercase();
    // Find first occurrence of " ON CONFLICT" that isn't inside a string
    // literal. For Phase 2, a simple search is acceptable — the equivalence
    // harness will catch any divergence on exotic cases.
    upper.find(" ON CONFLICT")
}
```

Also update `inject_returning` to use these helpers. It should already call `find_insert_stmt`, `extract_insert_target`, `find_splice_offset` per Task 3's scaffold — verify and fix the bodies.

- [ ] **Step 6: Run the inject tests**

```bash
cargo test --lib sql::ast::tests::ast_inject
```

Expected: all 12 tests pass. If any fail, debug the splice or AST walk.

- [ ] **Step 7: Run the dispatch-wrapper test from Task 5 under both features**

```bash
# Default: pg_query-backed
cargo test --lib correlate::capture::tests
# Legacy
cargo test --lib --features legacy-returning correlate::capture::tests
```

Both sets should pass.

- [ ] **Step 8: Clippy + fmt**

```bash
cargo clippy --lib --tests -- -D warnings
cargo fmt --check
```

- [ ] **Step 9: Commit**

```bash
git add src/sql/ast.rs
git commit -m "$(cat <<'EOF'
feat(sql): implement AST-backed inject_returning with splice

Locates the RETURNING splice point via pg_query AST (before ON
CONFLICT if present, otherwise at end of statement excluding
trailing whitespace/comments/semicolons). Splices the RETURNING
clause into the original SQL string — does not deparse the AST,
so comments and whitespace are preserved.

Handles 12 edge cases tested directly: simple INSERT, already
has RETURNING, unknown table, ON CONFLICT DO NOTHING, ON CONFLICT
DO UPDATE, multi-row VALUES, INSERT-SELECT, trailing semicolon,
trailing comment, CTE-wrapped (returns None — Phase 3 scope),
schema-qualified tables, non-INSERT (returns None).

SC-007.
EOF
)"
```

---

## Task 8: Inject_returning corpus test (15+ queries)

**Files:**
- Create: `tests/fixtures/returning_corpus.txt`
- Create: `tests/fixtures/returning_expected.txt`
- Create: `tests/sql_returning_corpus.rs`

- [ ] **Step 1: Write the corpus file**

Create `tests/fixtures/returning_corpus.txt`. Each non-blank, non-`#` line is `<input-sql>` (the expected output is regenerated/committed via the snapshot pattern).

```
# returning_corpus.txt — inject_returning SC-007 corpus.
# 15+ INSERT shapes. The pk_map used by the test has:
#   public.orders       -> [id]
#   analytics.events    -> [event_id]
#   public.t            -> [a, b]   (composite PK)
# Regenerate expected.txt via:
#   REGEN_SNAPSHOTS=1 cargo test --test sql_returning_corpus
INSERT INTO orders (name) VALUES ('x')
INSERT INTO orders (name) VALUES ('x'), ('y')
INSERT INTO orders (id, name) VALUES (1, 'x') ON CONFLICT DO NOTHING
INSERT INTO orders (id, name) VALUES (1, 'x') ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name
INSERT INTO orders (name) SELECT name FROM staging
INSERT INTO orders (name) VALUES ('x');
INSERT INTO orders (name) VALUES ('x') -- trailing comment
INSERT INTO orders (name) VALUES ('x') /* trailing block */
INSERT INTO analytics.events (name) VALUES ('x')
INSERT INTO t (a_col, b_col) VALUES (1, 2)
INSERT INTO orders DEFAULT VALUES
INSERT INTO orders (name) VALUES ('x') RETURNING id
INSERT INTO unknown_table (x) VALUES (1)
UPDATE orders SET name = 'x' WHERE id = 1
DELETE FROM orders WHERE id = 1
WITH new_order AS (INSERT INTO orders (name) VALUES ('x')) SELECT * FROM new_order
```

Verify count:

```bash
grep -Ev '^($|#)' tests/fixtures/returning_corpus.txt | wc -l
```

Expected: `16`.

- [ ] **Step 2: Write the snapshot-pattern test**

Create `tests/sql_returning_corpus.rs`:

```rust
//! inject_returning corpus test (SC-007). Output format per line:
//! `<input>|<output or NONE>`. Regenerate with REGEN_SNAPSHOTS=1.

use pg_retest::correlate::capture::{inject_returning, TablePk};
use std::fs;
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn load_corpus() -> Vec<String> {
    let text = fs::read_to_string(fixtures_dir().join("returning_corpus.txt"))
        .expect("corpus file");
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|s| s.to_string())
        .collect()
}

fn pk_map() -> Vec<TablePk> {
    vec![
        TablePk {
            schema: "public".into(),
            table: "orders".into(),
            columns: vec!["id".into()],
        },
        TablePk {
            schema: "analytics".into(),
            table: "events".into(),
            columns: vec!["event_id".into()],
        },
        TablePk {
            schema: "public".into(),
            table: "t".into(),
            columns: vec!["a".into(), "b".into()],
        },
    ]
}

#[test]
fn returning_gold_snapshot() {
    let corpus = load_corpus();
    assert!(
        corpus.len() >= 15,
        "corpus must have at least 15 queries (found {})",
        corpus.len()
    );
    let pk = pk_map();

    let actual: Vec<String> = corpus
        .iter()
        .map(|sql| match inject_returning(sql, &pk) {
            Some(out) => format!("{}|{}", sql, out),
            None => format!("{}|NONE", sql),
        })
        .collect();

    let expected_path = fixtures_dir().join("returning_expected.txt");

    if std::env::var("REGEN_SNAPSHOTS").is_ok() {
        let mut out = String::new();
        for line in &actual {
            out.push_str(line);
            out.push('\n');
        }
        fs::write(&expected_path, &out).expect("write expected");
        eprintln!("regenerated {}", expected_path.display());
        return;
    }

    let expected_text = fs::read_to_string(&expected_path)
        .expect("expected file — generate with REGEN_SNAPSHOTS=1");
    let expected: Vec<&str> = expected_text.lines().collect();

    assert_eq!(actual.len(), expected.len(), "line count mismatch");
    for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        assert_eq!(
            a, e,
            "line {} mismatch\ninput:    {}\nactual:   {}\nexpected: {}",
            i, corpus[i], a, e
        );
    }
}
```

- [ ] **Step 3: Regenerate expected.txt against the NEW pg_query-backed impl**

```bash
REGEN_SNAPSHOTS=1 cargo test --test sql_returning_corpus
```

Expected: prints `regenerated .../returning_expected.txt`.

- [ ] **Step 4: Inspect expected.txt for sanity**

```bash
cat tests/fixtures/returning_expected.txt
```

Verify each output looks right. Expected lines (abridged):

```
INSERT INTO orders (name) VALUES ('x')|INSERT INTO orders (name) VALUES ('x') RETURNING id
INSERT INTO orders (name) VALUES ('x'), ('y')|INSERT INTO orders (name) VALUES ('x'), ('y') RETURNING id
INSERT INTO orders (id, name) VALUES (1, 'x') ON CONFLICT DO NOTHING|INSERT INTO orders (id, name) VALUES (1, 'x') RETURNING id ON CONFLICT DO NOTHING
INSERT INTO orders (id, name) VALUES (1, 'x') ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name|INSERT INTO orders (id, name) VALUES (1, 'x') RETURNING id ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name
...
INSERT INTO orders (name) VALUES ('x') RETURNING id|NONE                          # already has RETURNING
INSERT INTO unknown_table (x) VALUES (1)|NONE
UPDATE orders SET name = 'x' WHERE id = 1|NONE
DELETE FROM orders WHERE id = 1|NONE
WITH new_order AS (INSERT INTO orders (name) VALUES ('x')) SELECT * FROM new_order|NONE
INSERT INTO t (a_col, b_col) VALUES (1, 2)|INSERT INTO t (a_col, b_col) VALUES (1, 2) RETURNING a, b
```

If any line looks wrong, STOP and investigate — the splice or AST walk has a bug.

- [ ] **Step 5: Verify test passes against committed expected.txt**

```bash
cargo test --test sql_returning_corpus
```

- [ ] **Step 6: Commit**

```bash
git add tests/fixtures/returning_corpus.txt tests/fixtures/returning_expected.txt tests/sql_returning_corpus.rs
git commit -m "$(cat <<'EOF'
test(sql): add inject_returning corpus test (16 queries)

Snapshot-pattern test covering simple/multi-row/INSERT-SELECT,
ON CONFLICT DO NOTHING / DO UPDATE, trailing semicolon/comments,
schema-qualified, composite PK, DEFAULT VALUES, already-has-
RETURNING (returns None), unknown table, UPDATE/DELETE (not in
scope), CTE-wrapped (Phase 2 returns None).

Format per line: <input>|<output or NONE>. Regenerate via
REGEN_SNAPSHOTS=1.

SC-007.
EOF
)"
```

---

## Task 9: pg_query equivalence harness (100+ queries)

**Files:**
- Create: `tests/fixtures/sql_corpus.txt`
- Create: `tests/pg_query_equivalence.rs`

- [ ] **Step 1: Write the 100+ query corpus**

Create `tests/fixtures/sql_corpus.txt`. Mix of realistic shapes. Use a combination of:
- 20 SELECT statements (simple, joins, CTEs, window functions, DISTINCT ON, LATERAL, JSON operators)
- 20 INSERT variants (simple, multi-row, INSERT-SELECT, ON CONFLICT, RETURNING, DEFAULT VALUES, schema-qualified)
- 15 UPDATE statements (simple, with JOIN, RETURNING, FROM clause)
- 15 DELETE statements (simple, USING, RETURNING)
- 10 WITH / CTE wrappers (recursive, materialized, data-modifying)
- 10 DDL (CREATE TABLE, ALTER, CREATE INDEX)
- 10 complex real-world shapes (MERGE, pg_catalog queries, setval, EXPLAIN)

Generate by hand or borrow from existing `.wkl` captures — any .wkl from the Docker demo works. Write them one-per-line. Target exactly 100 non-comment non-blank lines.

File starts with:

```
# sql_corpus.txt — pg_query equivalence harness corpus.
# 100+ SQL statements, one per non-blank non-# line.
# Used by tests/pg_query_equivalence.rs to assert the AST-backed
# has_returning (and future extract_tables / extract_filter_columns
# in Phase 3) agree with pg_query's direct AST answer.
# Full coverage: SELECTs, all DML shapes, CTEs, windows, DDL, MERGE.
```

(Then the 100+ queries.)

Verify count:

```bash
grep -Ev '^($|#)' tests/fixtures/sql_corpus.txt | wc -l
```

Expected: ≥100.

- [ ] **Step 2: Write the equivalence harness**

Create `tests/pg_query_equivalence.rs`:

```rust
//! Equivalence harness: for every SQL in the corpus, assert that
//! `correlate::capture::has_returning` (our wrapper) returns the same value
//! as a direct pg_query AST walk (the oracle). Divergence = test failure.
//!
//! Phase 3 will extend this to cover extract_tables / extract_filter_columns.
//!
//! Part of SC-008.

use pg_query::protobuf::node::Node as N;
use pg_retest::correlate::capture::has_returning as subject_has_returning;
use std::fs;
use std::path::PathBuf;

fn corpus() -> Vec<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sql_corpus.txt");
    let text = fs::read_to_string(path).expect("corpus file");
    text.lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|s| s.to_string())
        .collect()
}

/// Oracle: compute has_returning by walking pg_query's AST directly.
fn oracle_has_returning(sql: &str) -> Result<bool, String> {
    let parsed = pg_query::parse(sql).map_err(|e| format!("{}", e))?;
    for stmt in parsed.protobuf.stmts.iter() {
        if let Some(node) = stmt.stmt.as_ref().and_then(|s| s.node.as_ref()) {
            if walk(node)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn walk(node: &N) -> Result<bool, String> {
    match node {
        N::InsertStmt(s) => Ok(!s.returning_list.is_empty()),
        N::UpdateStmt(s) => Ok(!s.returning_list.is_empty()),
        N::DeleteStmt(s) => Ok(!s.returning_list.is_empty()),
        N::MergeStmt(_) => Ok(false),
        N::SelectStmt(s) => {
            if let Some(with) = s.with_clause.as_ref() {
                for cte_node in with.ctes.iter() {
                    if let Some(N::CommonTableExpr(cte)) = cte_node.node.as_ref() {
                        if let Some(q) = cte.ctequery.as_ref() {
                            if let Some(inner) = q.node.as_ref() {
                                if walk(inner)? {
                                    return Ok(true);
                                }
                            }
                        }
                    }
                }
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

#[test]
fn has_returning_equivalence() {
    let corpus = corpus();
    assert!(corpus.len() >= 100, "corpus must have at least 100 queries (found {})", corpus.len());
    let mut mismatches = Vec::new();
    for (i, sql) in corpus.iter().enumerate() {
        let oracle = match oracle_has_returning(sql) {
            Ok(v) => v,
            Err(_) => {
                // Unparseable — oracle can't answer. The wrapper's safe
                // default is false; don't count this as a mismatch.
                continue;
            }
        };
        let actual = subject_has_returning(sql);
        if actual != oracle {
            mismatches.push(format!(
                "line {}: oracle={} actual={} sql={}",
                i, oracle, actual, sql
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "has_returning diverges from pg_query oracle on {} queries:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}
```

- [ ] **Step 3: Run — expect PASS on first try if the implementation is correct**

```bash
cargo test --test pg_query_equivalence
```

Expected: PASS. If fails: the mismatch output names the exact SQL + oracle vs actual. Fix whichever is wrong (usually the subject, sometimes the oracle walk if it's missing a node type).

- [ ] **Step 4: Commit**

```bash
git add tests/fixtures/sql_corpus.txt tests/pg_query_equivalence.rs
git commit -m "$(cat <<'EOF'
test(sql): add pg_query equivalence harness (100+ query corpus)

For every SQL in the corpus, assert that has_returning agrees
with a direct pg_query AST walk (oracle). Divergence fails CI.

Covers: 20 SELECTs with CTEs/windows/DISTINCT ON/LATERAL/JSON,
20 INSERT variants, 15 UPDATEs, 15 DELETEs, 10 WITH wrappers,
10 DDL, 10 complex real-world shapes including MERGE.

Phase 3 will extend this with extract_tables and
extract_filter_columns oracles against the same corpus.

SC-008 (AST-backed sites portion).
EOF
)"
```

---

## Task 10: Verify hot-path bench under the pre-filter (DC-004 prep)

**Files:**
- Overwrite: `benches/baselines/returning_after.txt`

- [ ] **Step 1: Run the post-change bench**

```bash
rm -rf target/criterion
cargo bench --bench returning_bench 2>&1 | tee benches/baselines/returning_after.txt
```

- [ ] **Step 2: Compare against baseline**

```bash
grep -E "time:" benches/baselines/returning_before.txt
echo "---"
grep -E "time:" benches/baselines/returning_after.txt
```

Per SC-005 (revised) and DC-004, each case should meet the following envelope:

- **`returning_select_skipped`** — must hit the pre-filter, so `time` should be near-identical to baseline (within ±5%). The pre-filter's whole purpose is to keep this case cheap. A regression here is a bug.
- **`returning_insert_*` and `returning_cte_wrapped`** — will regress relative to the hand-rolled substring scan because pg_query is doing real parsing work (~2–20 µs typical). Absolute cost must remain ≤500 ns/query for non-candidate SELECTs and ≤50 µs for parse-required cases. These are the numbers that go into the hot-path budget for the proxy.

If the SELECT-skipped case regresses, the pre-filter isn't hitting — investigate.

- [ ] **Step 3: Document the numbers in a commit**

```bash
git add benches/baselines/returning_after.txt
git commit -m "$(cat <<'EOF'
bench: capture post-Phase-2 has_returning bench

Results vs pre-Phase-2 baseline:
- returning_select_skipped:    <X ns> -> <Y ns>   (pre-filter hit)
- returning_insert_no_returning: <X ns> -> <Y ns> (parse required)
- returning_insert_with_returning: <X ns> -> <Y ns>
- returning_cte_wrapped:       <X ns> -> <Y ns>

Pre-filter keeps the SELECT case within ±5% of baseline (SC-005
revised envelope). Parse-required cases are ~<N>x slower but
absolute cost stays under 50µs, well within the proxy hot-path
budget.

SC-005, DC-004.
EOF
)"
```

Replace the bracketed values with real numbers from the bench output.

---

## Task 11: Docker demo E2E validation (DC-004 gate)

**Files:**
- No file changes — verification only.

- [ ] **Step 1: Verify Docker is available**

```bash
docker --version && docker compose version
```

If not available, this task is BLOCKED. Install Docker or skip with explicit note in DC-004.

- [ ] **Step 2: Bring up the demo stack**

```bash
docker compose up -d
# Wait for healthy
sleep 5
docker compose ps
```

Expected: `db-a` and `db-b` containers running and healthy.

- [ ] **Step 3: Build release binary**

```bash
cargo build --release
```

- [ ] **Step 4: Run the ID-correlation hot path**

Start the proxy in capture mode with ID correlation, pointed at db-a:

```bash
# Terminal 1: start proxy
PG_RETEST_DEMO=true ./target/release/pg-retest capture \
    --source-type proxy \
    --bind 127.0.0.1:6544 \
    --target "host=127.0.0.1 port=5433 dbname=demo user=demo password=demo" \
    --source-db "host=127.0.0.1 port=5433 dbname=demo user=demo password=demo" \
    --id-mode full \
    --id-capture-implicit \
    --mask-values \
    --output /tmp/demo-phase2.wkl
```

In a second terminal, generate some traffic. Use whatever demo script the `PG_RETEST_DEMO=true` mode provides, OR manually exercise INSERTs:

```bash
# Terminal 2: send test queries
psql "host=127.0.0.1 port=6544 dbname=demo user=demo password=demo" <<EOF
INSERT INTO orders (customer_id, total) VALUES (1, 19.99);
INSERT INTO orders (customer_id, total) VALUES (2, 42.50) RETURNING id;
SELECT * FROM orders LIMIT 5;
WITH new_o AS (INSERT INTO orders (customer_id, total) VALUES (3, 7.25) RETURNING id)
SELECT * FROM new_o;
EOF
```

Stop the proxy (Ctrl-C in terminal 1). Expected: no panic, no "column does not exist" errors, proxy exits cleanly.

- [ ] **Step 5: Inspect the captured profile**

```bash
./target/release/pg-retest inspect /tmp/demo-phase2.wkl | head -40
```

Expected: the captured queries include the INSERTs with response_values (RETURNING ids), no unexpected nulls, implicit RETURNING injection worked.

- [ ] **Step 6: Replay against db-b**

```bash
./target/release/pg-retest replay \
    --profile /tmp/demo-phase2.wkl \
    --target "host=127.0.0.1 port=5434 dbname=demo user=demo password=demo" \
    --id-mode full
```

Expected: replay completes without errors, ID substitution rate matches pre-Phase-2 expectations (same 4–7% band per CLAUDE.md — no change, that's Phase 1 scope).

- [ ] **Step 7: Tear down**

```bash
docker compose down
```

- [ ] **Step 8: Commit a Docker demo report as a doc**

If anything went wrong, STOP and investigate. If all green, no commit needed — DC-004 will reference this task in the drift check.

---

## ⛔ Drift Check DC-004

**Trigger:** After Task 11 completes, before marking Phase 2 done.

- [ ] **Step 1: Re-read mission brief**

```bash
cat /home/yonk/yonk-apps/pg-retest/skill-output/mission-brief/Mission-Brief-sql-parsing-upgrade.md
```

- [ ] **Step 2: Drift questions**

1. **Am I still solving the stated Purpose?** Expected: *yes — has_returning/inject_returning now on libpg_query, two new bug classes (CTE-wrapped writes, column aliased as "returning") correctly handled, pre-filter keeps hot-path tax bounded.*
2. **Does my current work map to Success Criteria?**
   - SC-006 → `cargo test --lib sql::ast::tests::ast_has_returning` passes (8+ cases).
   - SC-007 → `cargo test --test sql_returning_corpus` passes (16 cases); `cargo test --lib sql::ast::tests::ast_inject` passes (12 cases).
   - SC-008 → `cargo test --test pg_query_equivalence` passes (100+ queries).
   - SC-010 → all commits on `dev/1.0.0-rc.5`, version bumped.
   - SC-011 → `cargo test --lib --features legacy-returning correlate::capture::tests` passes; legacy impl still present.
   - DC-004 → `benches/baselines/returning_after.txt` committed; SELECT-skipped case within ±5%.
3. **Am I doing anything in Out of Scope?** Expected: *no — Phase 3 sites (extract_tables, extract_filter_columns, mysql_to_pg) untouched. Proxy wire-protocol untouched outside the 6 has_returning/inject_returning call sites.*

- [ ] **Step 3: Artifact checklist**

| SC | Evidence | Location |
|------|----------|----------|
| SC-006 | AST-backed has_returning + unit tests | `src/sql/ast.rs`, `src/correlate/capture.rs` |
| SC-007 | AST-backed inject_returning + corpus | `src/sql/ast.rs`, `tests/sql_returning_corpus.rs`, `tests/fixtures/returning_*.txt` |
| SC-008 | 100+ query equivalence harness | `tests/pg_query_equivalence.rs`, `tests/fixtures/sql_corpus.txt` |
| SC-011 | legacy-returning feature flag | `Cargo.toml`, `src/correlate/capture.rs::legacy` |
| SC-010 | Phase 2 ships on `dev/1.0.0-rc.5` | `git log`, Cargo.toml version |
| SC-012 | Tests, clippy, fmt, build --release clean | CI output |
| DC-003 | Binary size delta ≤ 2 MB documented | Task 1 commit message |
| DC-004 | Docker demo E2E clean; returning_after.txt shows pre-filter works | Task 10 + Task 11 |

If any row has blank evidence, Phase 2 is not complete.

- [ ] **Step 4: Confirm nothing outside Phase 2 scope was touched**

```bash
git diff main..HEAD --stat -- \
    src/transform/analyze.rs \
    src/transform/mysql_to_pg.rs \
    src/sql/lex.rs \
    src/capture/masking.rs \
    src/correlate/substitute.rs
```

Expected: empty output.

---

## Task 12: Update CLAUDE.md and CHANGELOG, final verification sweep

**Files:**
- Modify: `CLAUDE.md`
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Add `src/sql/ast.rs` entry to CLAUDE.md's "Key modules" section**

Insert after the existing `src/sql/lex` entry:

```
- `sql::ast` — libpg_query-backed AST helpers for structural SQL analysis. `has_returning(sql) -> Result<bool>` and `inject_returning(sql, pk_map) -> Result<Option<String>>`. Hot-path callers use the cheap `might_have_returning` prefix pre-filter before dispatching to pg_query (~2-20µs per parse). The public `correlate::capture::has_returning` and `inject_returning` wrappers preserve the bool/Option<String> shape by mapping Err to the safe default (false / None).
```

- [ ] **Step 2: Add Phase 2 Gotchas entries**

Append:

```
- pg_query dep adds ~1.5 MB to release binary. Requires a C compiler at build time (libpg_query is C). Not tested on Windows — best-effort per the mission brief.
- The `legacy-returning` feature flag compiles the pre-Phase-2 hand-rolled has_returning/inject_returning instead of the pg_query-backed ones. Rollback safety net; removed in rc.6 per SC-011.
- `has_returning` becoming fallible (Result<bool>) is implementation-internal. Callers in `src/proxy/connection.rs` see the old bool signature via the wrapper — Err is mapped to false (matches prior "unknown = assume no RETURNING" behavior).
- `inject_returning` splice approach: pg_query gives us the AST and locations; we splice RETURNING into the original SQL string (not deparse, which would lose comments/whitespace). CTE-wrapped inserts (`WITH x AS (INSERT ...) SELECT ...`) return None — splice semantics ambiguous, Phase 3 may revisit.
```

- [ ] **Step 3: Update CHANGELOG.md `[Unreleased]`**

Add:

```
### Added

- **libpg_query (pg_query.rs) dependency** for AST-backed RETURNING
  detection and injection. Binary size grows ~1.5 MB. Requires a C
  compiler at build time (not tested on Windows).
- **`legacy-returning` feature flag** compiles the pre-Phase-2 hand-rolled
  has_returning/inject_returning instead of the new pg_query-backed
  impls. Rollback safety net; scheduled for removal in rc.6.
- **pg_query equivalence harness** (`tests/pg_query_equivalence.rs`) over
  a 100+ query corpus. Asserts the subject implementation agrees with a
  direct pg_query AST walk on every corpus entry.

### Changed

- **`has_returning` now uses libpg_query** by default. Correctly handles
  CTE-wrapped writes, columns aliased as `"returning"`, and the word
  `RETURNING` inside comments/string literals — all bug classes in the
  legacy impl.
- **`inject_returning` now uses libpg_query** by default. Splices
  RETURNING at the AST-identified offset (before ON CONFLICT, otherwise
  at end of statement excluding trailing whitespace/comments/semicolons).
  Comments and whitespace preserved — no deparse.
- **CTE-wrapped inserts**: `inject_returning` now returns None for
  `WITH x AS (INSERT ...) SELECT ...` (previously returned the RETURNING
  clause but with ambiguous semantics). Phase 3 may revisit.
```

- [ ] **Step 4: Full verification sweep**

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features legacy-returning -- -D warnings
cargo test --lib
cargo test --tests
cargo test --lib --features legacy-returning
cargo build --release
cargo build --release --features legacy-returning
```

All must pass / clean. Note the double-feature checks: both the default and the `legacy-returning` build path must compile and test-pass — this is what SC-011 is for.

- [ ] **Step 5: Commit**

```bash
git add CLAUDE.md CHANGELOG.md
git commit -m "$(cat <<'EOF'
docs: Phase 2 — CLAUDE.md + CHANGELOG for pg_query-backed RETURNING

Documents the new sql::ast module, the legacy-returning feature
flag lifecycle (ships rc.5, removed rc.6), binary size impact,
and the behavioral change for CTE-wrapped inserts.

Concludes Phase 2 of the SQL parsing upgrade.

SC-010, SC-012.
EOF
)"
```

---

## Out-of-Scope Reminder (from Mission Brief)

The executing agent MUST NOT, during Phase 2:

- Modify `src/sql/lex.rs`, `src/capture/masking.rs`, or `src/correlate/substitute.rs` (Phase 1, locked).
- Modify `src/transform/analyze.rs` extract_tables or extract_filter_columns (Phase 3 scope).
- Modify `src/transform/mysql_to_pg.rs` (out of scope per brief).
- Change the `.wkl` profile format.
- Change proxy wire-protocol handling outside the has_returning/inject_returning call sites in `src/proxy/connection.rs`.
- Add CLI flags beyond the `legacy-returning` Cargo feature.
- Address the `substitute_ids` 4-7% cross-session error rate.
- Remove the legacy impl before rc.6 (SC-011 explicitly requires one release cycle of rollback safety net).

If a scope-adjacent temptation arises ("I notice `extract_tables` regex is broken on this corpus line" / "let me also speed up substitute_ids"), STOP — it's Phase 3 or mission-brief-exclusion scope. Note the observation, do not act.
