# Experiment Report — `polyglot-sql` AST transpiler behind `SqlTransformer`

**Branch:** `experimental/polyglot-transform` (off `main`)
**Status:** Complete — P0 + P1 slice, experimental, feature-flagged OFF by default.
**Spec implemented:** `retest-research/plan/include/polyglot-sql-transform.md` (P0 + P1), per
the build sequence in `retest-research/plan/00-overview.md`.

---

## 1. What this experiment is

pg-retest's MySQL→PostgreSQL transform is **seven hand-written regex rules**
(`src/transform/mysql_to_pg.rs`). Regex doesn't parse SQL: it can rewrite an `IF(`
inside a string *literal*, it can't reject MySQL syntax that PostgreSQL doesn't
accept, and it only knows the ~7 functions someone hand-coded.

This experiment integrates the MIT-licensed [`polyglot-sql`](https://github.com/tobilg/polyglot)
crate (a pure-Rust ~32-dialect SQL transpiler) **behind pg-retest's existing
`SqlTransformer` trait** as a new `PolyglotTransformer`, with a strict
**"translate-faithfully or flag-and-skip, never silently mistranslate"** posture. The
legacy regex pipeline stays as a labelled, reversible fallback. Everything is gated
behind the `polyglot-transform` Cargo feature, **OFF by default**.

## 2. How to enable the feature

The default build is unchanged — PG-only, no new dependency linked:

```bash
cargo build            # polyglot-sql NOT compiled or linked
cargo test             # existing behavior, unchanged
```

Turn the experiment on with the feature flag:

```bash
cargo build  --features polyglot-transform
cargo test   --features polyglot-transform
# see the before/after benefit table:
cargo test   --features polyglot-transform --test polyglot_benefit_harness -- --nocapture
```

`Cargo.toml` (the dependency is `optional`, so it is only pulled when the feature is on):

```toml
[features]
polyglot-transform = ["dep:polyglot-sql"]

[dependencies]
polyglot-sql = { version = "0.5", path = "../retest-research/polyglot/crates/polyglot-sql",
                 default-features = false,
                 features = ["transpile", "stacker", "dialect-postgresql", "dialect-mysql"],
                 optional = true }
```

> **Dependency sourcing (notable choice):** this experiment uses a **path dependency**
> to the verified local 0.5.4 clone — the exact source every API claim was checked
> against — which eliminates the spec's #1 risk (pre-1.0 crates.io API drift) for a
> reproducible experiment. To productionize, delete `path = …` and keep
> `version = "0.5"` (the published crate). One-line swap. The branch is experimental
> and not for merge to `main`.

## 3. What was built

| File | Change |
|---|---|
| `src/profile/mod.rs` | **P0:** `SourceDialect` enum + `WorkloadProfile.source_dialect` + `Query.original_sql`, both `#[serde(default)]`, both **trailing** (see §6). |
| `src/transform/polyglot.rs` | **P1:** `PolyglotTransformer` (impl `SqlTransformer`) + `mysql_to_pg_polyglot_pipeline()`. |
| `src/transform/dialect.rs` | **P1:** `to_polyglot(SourceDialect) -> DialectType` (note `PostgreSQL`, not `Postgres`). |
| `src/transform/mod.rs` | feature-gated `mod` wiring + `pub fn is_valid_postgres()` (the pg_query validity check). |
| `src/capture/mysql_slow.rs` | stamps `source_dialect = MySql`; `apply_transform` preserves the dialect. |
| ~150 construction sites | mechanical, compiler-driven additive field updates (codemod). |
| `tests/profile_io_test.rs` | backward-compat test: an old-shaped `.wkl` decodes with the new fields defaulting. |
| `tests/polyglot_integration_test.rs` | capture → polyglot pipeline end-to-end (feature-gated). |
| `tests/polyglot_benefit_harness.rs` | the regex-vs-transpiler benefit table (feature-gated). |
| `NOTICE` | MIT notices for polyglot-sql + its upstream model sqlglot (FR-XFORM-3). |

### The honesty posture (three layers)

`PolyglotTransformer::transform()` returns `Transformed` **only** when all hold:

1. **`TranspileOptions::strict()`** (`UnsupportedLevel::Raise`), *not* the crate default
   `Warn` (which emits lossy output silently). Parse/tokenize/syntax/unsupported errors
   → `Skipped{reason}`.
2. **Single statement** — multi-statement (or zero-statement) transpile output →
   `Skipped` (never joined; joining corrupts statement boundaries, timing, row-count
   attribution, and txn correlation at replay).
3. **pg_query validity gate** — the transpiled output is re-parsed by PostgreSQL's own
   parser (libpg_query, already a pg-retest dependency). Anything PG rejects →
   `Skipped`. **This layer was added (with approval) beyond the spec snippet** because
   of an empirical finding (§5).

## 4. Validation A — correctness (real output)

**Build + lint gates** (both feature states):

```
-- build (default features) --                Finished `dev` profile … in 4.28s
-- build (--features polyglot-transform) --   Finished `dev` profile … in 11.79s
-- clippy (default) -D warnings --            Finished `dev` profile … in 0.16s
-- clippy (--features polyglot-transform) --  Finished `dev` profile … in 2.76s
-- fmt --check --                             fmt: clean
```

**Existing suite, no regressions** (`cargo test --lib --tests`, total tests passed):

```
feature OFF (default): 517 passed, 0 failed
feature ON           : 530 passed, 0 failed   (+13 new gated tests)
```

> One pre-existing **doctest** failure (`src/sql/ast.rs:188`, the RETURNING-splice
> doc comment) is present on `main` and **unrelated** to this work — see §7. The
> counts above use `--lib --tests` (the meaningful "did my change break anything"
> gate); it is the only difference between `cargo test` and `cargo test --lib --tests`.

**P0 backward-compatibility** — an *old-shaped* profile (no new fields), serialized with
the same MessagePack array encoding `profile::io` uses, decodes as the current type:

```
test test_old_wkl_without_new_fields_deserializes_with_defaults ... ok
test test_new_profile_roundtrips_source_dialect_and_original_sql ... ok
test result: ok. 11 passed; 0 failed; …
```

**P1 transformer unit tests** (13, feature ON) — parity + the headline + skip/gate:

```
test transform::dialect::tests::test_dialect_mapping_targets_postgresql_not_postgres ... ok
test transform::polyglot::tests::test_ifnull_to_coalesce ... ok
test transform::polyglot::tests::test_backtick_identifiers_become_double_quoted ... ok
test transform::polyglot::tests::test_limit_offset_rewrite ... ok
test transform::polyglot::tests::test_unix_timestamp_rewrite_is_valid_pg ... ok
test transform::polyglot::tests::test_if_inside_string_literal_is_preserved ... ok
test transform::polyglot::tests::test_identity_source_equals_target_is_unchanged ... ok
test transform::polyglot::tests::test_unparseable_source_is_skipped ... ok
test transform::polyglot::tests::test_multi_statement_output_is_skipped_not_joined ... ok
test transform::polyglot::tests::test_pg_invalid_passthrough_is_skipped_by_validity_gate ... ok
test transform::polyglot::tests::test_every_transformed_output_reparses_as_valid_pg ... ok
test transform::polyglot::tests::test_mysql_to_pg_polyglot_pipeline_transforms ... ok
test transform::polyglot::tests::test_name_is_polyglot ... ok
```

**P1 integration test** (capture → transform, feature ON):

```
test test_capture_then_polyglot_transform_end_to_end ... ok
```

## 5. Validation B — the benefit (real output)

`cargo test --features polyglot-transform --test polyglot_benefit_harness -- --nocapture`

```
  MySQL → PostgreSQL: regex pipeline vs. polyglot transpiler

  construct                                | regex         | polyglot
  ------------------------------------------------------------------------
  backtick identifiers                     | correct       | correct
  LIMIT offset,count                       | correct       | correct
  IFNULL→COALESCE                          | correct       | correct
  IF()→CASE                                | correct       | correct
  UNIX_TIMESTAMP()                         | correct       | correct
  MySQL internal (SHOW)                    | skipped       | skipped
  MySQL internal (SET NAMES)               | skipped       | skipped
  already PG-compatible                    | correct       | correct
  plain DML                                | correct       | correct
  IF( inside string literal [HEADLINE]     | MISTRANSLATED | correct
  IFNULL( inside string literal            | MISTRANSLATED | correct
  AUTO_INCREMENT                           | MISTRANSLATED | correct
  ON DUPLICATE KEY UPDATE                  | MISTRANSLATED | skipped
  backslash string escape                  | MISTRANSLATED | correct
  DATE_FORMAT                              | correct       | correct
  CONCAT                                   | correct       | correct
  GROUP_CONCAT                             | MISTRANSLATED | correct

  totals (17 constructs):
    regex    → correct  9 | skipped  2 | MISTRANSLATED  6
    polyglot → correct 14 | skipped  3 | MISTRANSLATED  0

  mistranslation details:
    regex    IF( inside string literal [HEADLINE]   corrupted literal → SELECT 'CASE WHEN x THEN 1 ELSE 0 END'
    regex    IFNULL( inside string literal          corrupted literal → SELECT 'COALESCE(a,b)' AS lit
    regex    AUTO_INCREMENT                         invalid PG → CREATE TABLE t (id INT AUTO_INCREMENT PRIMARY KEY)
    regex    ON DUPLICATE KEY UPDATE                invalid PG → INSERT INTO t (id) VALUES (1) ON DUPLICATE KEY UPDATE id = id
    regex    backslash string escape                invalid PG → SELECT name FROM t WHERE x = 'a\'b'
    regex    GROUP_CONCAT                           invalid PG → SELECT GROUP_CONCAT(name SEPARATOR ',') FROM t
```

**The headline guarantee: the transpiler's MISTRANSLATED count is 0.** The harness
test *asserts* `polyglot_mistranslated == 0` and `regex_mistranslated >= 1`, so a
regression makes CI red.

`MISTRANSLATED` means the engine **emitted** output (did not flag-and-skip) that is
either:
- **silently semantically corrupt** — valid SQL that *runs and returns wrong data*. The
  two string-literal rows are the dangerous case: regex's `IfToCase`/`IfnullToCoalesce`
  rewrite the text *inside* a string literal, changing `'IF(x,1,0)'` into the string
  `'CASE WHEN x THEN 1 ELSE 0 END'`. A migration validator using this would "prove" a
  database state production never produced. The AST transpiler cannot do this — string
  literals are opaque tokens it never descends into; or
- **invalid PG** — fails loudly on replay (`AUTO_INCREMENT`, `ON DUPLICATE KEY UPDATE`,
  backslash escapes, `GROUP_CONCAT`). Regex passes MySQL syntax straight through; the
  transpiler either translates it (`GENERATED … AS IDENTITY`, `STRING_AGG`) or honestly
  skips it (`ON DUPLICATE KEY UPDATE → skipped`).

## 6. Spec deviations (surfaced, not silently taken)

1. **Field placement — `source_dialect` is TRAILING, not mid-struct.** The spec
   (FR-XFORM-11) drafted `source_dialect` between `capture_method` and `sessions` and
   asserted old `.wkl` files still load. **That is false under the encoding pg-retest
   actually uses:** `profile::io` calls `rmp_serde::to_vec` (MessagePack *array*/
   positional), where `#[serde(default)]` only rescues **trailing** missing fields. A
   mid-struct field misaligns positional decoding of every existing capture file. Fixed
   by appending the field; proven by `test_old_wkl_without_new_fields_deserializes_with_defaults`.
2. **pg_query validity gate added to the transformer (user-approved).** The spec claims
   `strict()` means "untranslatable → flagged-and-skipped." Verified empirically against
   polyglot 0.5.4: `strict()` reliably prevents string-literal mangling and catches
   parse errors, but it does **not** raise on many MySQL-specific constructs that are
   invalid in PostgreSQL — it passes `ON DUPLICATE KEY UPDATE`, `REPLACE INTO`,
   `USE INDEX`, `STRAIGHT_JOIN`, multi-table `DELETE`, etc. through unchanged. Without a
   gate the transformer would emit invalid PG as "translated." The gate re-parses output
   with PG's own parser and skips anything it rejects → the ~0-mistranslation guarantee
   holds *by construction*, not by trusting the upstream `Raise` level.
3. **Profile `version` NOT bumped** (ASK-FIRST gate). The change is additive and
   backward-compatible, so the `version: u8` constant is unchanged — old and new files
   interoperate without a version bump.

## 7. Honest limitations

- **Syntactic validity ≠ behavioral equivalence.** The validity gate (and the harness's
  "correct") prove the output *parses* as PostgreSQL. They do **not** prove faithful
  runtime behavior. `DATE_FORMAT` and `CONCAT` show "correct" for both engines only
  because `pg_query` accepts the function-call *syntax*; MySQL-vs-PG semantic
  differences (CONCAT null handling, autocommit, `LAST_INSERT_ID`, zero-dates,
  `TINYINT(1)`↔bool, collation) are an explicit non-goal (spec §1) and are **not**
  caught here. A real migration needs the per-dialect accuracy doc the spec defers.
- **The transpiler is not strictly "better" everywhere — and the table says so.** For
  the `IF()`-as-a-function form, regex rewrites to `CASE` while polyglot passes `IF(...)`
  through; both happen to be accepted by `pg_query` here. For `ON DUPLICATE KEY UPDATE`
  polyglot *skips* rather than translating to `ON CONFLICT`. The win is **zero silent
  mistranslations**, not universal superiority.
- **polyglot-sql is pre-1.0 (0.5.4).** API/behavior may churn. Mitigated by the pinned
  path/`version = "0.5"`, the committed `Cargo.lock`, and the harness (a behavior
  tripwire on upgrade).
- **Scope held to the experiment.** No `--transform-engine` CLI flag, no flip of the
  MySQL→PG default to polyglot, no transaction-aware skip, no `inspect`/`compare`
  surfacing of `source_dialect`. Those are the spec's full-P1 items, intentionally out
  of this experimental slice.
- **Pre-existing, unrelated failure:** `cargo test` (bare) is red on a **doctest** at
  `src/sql/ast.rs:188` — a 4-space-indented `INSERT … VALUES (…)` in the
  `find_splice_offset` doc comment that rustc tries to compile. It is present on `main`
  (`git diff main -- src/sql/ast.rs` is empty) and untouched here. Left alone per the
  "don't clean up unrelated code" constraint; one-line fix recommended below.

## 8. Recommended next steps

1. **Fix the pre-existing doctest** at `src/sql/ast.rs:188` (fence the example as
   ` ```text `), so bare `cargo test` / CI is green again. One line, unrelated to this
   experiment — left untouched here by scope rule.
2. **Productionize the dependency:** swap the path dep for the published
   `polyglot-sql = "0.5"` from crates.io once availability/version are confirmed.
3. **Full P1** (if promoting past experiment): `--transform-engine polyglot|regex`,
   transaction-aware skip (drop the whole transaction when any statement is skipped —
   FR-XFORM-8), provenance label + `original_sql` retention through capture, and
   `source_dialect`/skip-count surfacing in `inspect`/`compare` (FR-XFORM-13).
4. **Differential-oracle harness** (`retest-research/plan/reimplement/03-…`): replace
   this bespoke benefit harness with pg-retest's own compare machinery — transform a
   MySQL workload, replay on a containerized PG, diff vs. a reference — to validate
   *behavior*, not just syntax.
5. **Multi-dialect:** add `dialect-oracle`/`-tsql`/`-snowflake` features + capture
   sources to unlock Oracle/SQL-Server/Snowflake → PG "for free" through the same seam.
```
