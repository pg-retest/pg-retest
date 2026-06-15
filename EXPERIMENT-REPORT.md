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
4. **Differential-oracle harness** (`retest-research/plan/reimplement/03-…`): validate
   *behavior*, not just syntax. **→ Phase 1 of this is now built — see §9.**
5. **Multi-dialect:** add `dialect-oracle`/`-tsql`/`-snowflake` features + capture
   sources to unlock Oracle/SQL-Server/Snowflake → PG "for free" through the same seam.
```

## 9. Update — oracle-verified multi-pass translation (Phase 1, 2026-06-15)

Next-step #4 is now realized as a first slice. Translation became **verified search**:
candidate generators feed a **differential oracle** that *executes* each candidate on a
real PostgreSQL and diffs the result against an author-verified reference, accepting
only behavior-preserving candidates. This upgrades the guarantee from **syntactic** (the
`pg_query` parse gate) to **behavioral** (PostgreSQL runs it and returns the right rows).

- **Code:** `src/transform/oracle/{mod,normalize,engine,golden,corpus}.rs` (feature-gated
  `polyglot-transform`). `Verdict` = `Equivalent | Divergent | Error`; the engine
  cascades generators cheapest-first and accepts the first `Equivalent`, recording every
  attempt. `GoldenOracle` wraps each query as `SELECT _s::text FROM (sql) _s` so PG
  renders rows positionally — type- and column-name-agnostic, no per-type extraction.
- **Truth** = the author-verified correct PG translation (`corpus.toml`), run live on the
  seeded PG. No stored result blobs. Live-MySQL differential is Phase 2.
- **Gating:** `PG_RETEST_ORACLE_URL` (default local); every DB test skips cleanly with no
  PostgreSQL, so CI stays green.

**Benchmark, run on PostgreSQL 16 (real output):**

```
  case                 | verified | winner   | runners-up verdicts
  --------------------------------------------------------------------------
  ifnull_coalesce      | yes      | polyglot |
  backticks            | yes      | polyglot |
  limit_offset         | yes      | polyglot |
  string_literal_if    | yes      | polyglot |
  if_function          | yes      | regex    | polyglot=Error { detail: "db error" }
  wins by method: {"polyglot": 4, "regex": 1}   unverifiable/skipped: 0
```

**Why this is the whole argument for behavioral verification:** `if_function` —
polyglot's `IF(...)` passthrough **passed the syntactic `pg_query` gate** (PG's parser
accepts it as a function call) so the shipped transformer would have called it
"Transformed/valid", but on **execution** PostgreSQL errored (no `IF` function) →
`Error` → polyglot declined → the engine fell through to regex, which correctly emits
`CASE`. On `string_literal_if` the roles reverse (regex corrupts the literal, polyglot
preserves it). **Neither engine alone is correct across both queries; the oracle picks
the right one per query** — proven by execution, not asserted.

**Tests:** 14 oracle unit tests + 4 PG-gated integration tests + the benchmark; clippy
clean and suite green with the feature OFF and ON. **Spec/plan:**
`docs/superpowers/{specs,plans}/2026-06-15-oracle-verified-translation*`.

**Phase 2a — heterogeneous oracle-verified generators (built, 2026-06-15).** Generators
are now an async `CandidateGenerator` trait, so deterministic transpilers and external/
nondeterministic tools plug into the same engine. Added the **LLM generator**
(`src/transform/oracle/llm.rs`, reqwest → any OpenAI-compatible endpoint; env-config
`PG_RETEST_LLM_URL`/`MODEL`/`KEY`; live providers opt-in). The headline claim is proven
*deterministically*, no live LLM required:

- **Safety proof** (`engine.rs::test_flaky_generator_is_rejected_engine_recovers`): a
  generator that emits wrong SQL is caught by the oracle (`Divergent`) and the engine
  recovers with the next generator — wrong output is **never trusted**. This is exactly
  what makes a nondeterministic LLM as safe as a buggy regex rule.
- **LLM HTTP path proven** against a mock OpenAI endpoint
  (`llm.rs::test_llm_generator_parses_candidate_from_mock_endpoint`) and it declines
  gracefully (not panics) when the endpoint is unreachable.

19 oracle unit tests; clippy clean and suite green feature OFF/ON.

**Phase 2b — live-MySQL differential oracle (built, 2026-06-15).** The truest oracle:
run the *original* query on a real MySQL and the *candidate* on a real PostgreSQL, then
diff. `src/transform/oracle/live.rs` — `LiveDiffOracle` implements the same `Oracle`
trait, so the multi-pass engine consumes it with **zero change**; only the truth source
differs (live MySQL execution instead of an author-verified reference). MySQL is reached
via its CLI (no Rust driver dependency — the aws/bedrock external-tool pattern),
configured by `PG_RETEST_MYSQL_CMD`. The crux — **cross-engine result normalization** —
is in `normalize.rs` (`parse_pg_record` / `parse_mysql_row`): it reconciles the two
engines' genuinely-different text output (PG record-text `(1,alice,,"a,b")` vs MySQL
tab-separated; NULL-vs-`''`), and is covered by 6 pure unit tests (the false-diff guard)
*plus* validated end-to-end against real engines.

Proven on real MySQL 8.0 + PostgreSQL 16 (`tests/oracle_live_mysql_test.rs`):

```
live-diff oracle: all 5 corpus translations Equivalent vs real MySQL; wrong candidate caught.
```

Every author-verified translation behaviorally matched its MySQL original across the two
engines, a deliberately-wrong candidate was caught, and the full multi-pass engine —
refereed by the *live* oracle — picked the same engine per query as the golden benchmark
(polyglot the string-literal case, regex `IF()`→`CASE`), now confirmed by **execution on
real MySQL vs real PostgreSQL**.

**Phase 2c — sqlglot generator (built, 2026-06-15).** A genuine third tool:
`src/transform/oracle/sqlglot.rs` invokes the mature Python `sqlglot` transpiler as a
subprocess (no Rust binding — same external-tool pattern; Python configured via
`PG_RETEST_SQLGLOT_PYTHON`, declines gracefully if `import sqlglot` fails). sqlglot is
*more complete* than the 0.5.4 Rust port — it translates MySQL `IF(c,a,b)` →
`CASE WHEN …`, exactly the case polyglot passes through and PG rejects at execution.

Proven on live PG (`tests/oracle_sqlglot_test.rs`, sqlglot 30.11.0 via a uv venv):

```
sqlglot won `if_function` (polyglot declined: IF() fails on PG execution).
```

In the cascade `[polyglot, sqlglot]`, polyglot's `IF()` passthrough is rejected by the
oracle (execution error) and **sqlglot wins** — added coverage, picked up automatically
by verified search, no engine change.

**Phase 2d — writes/DML oracle (built, 2026-06-15).** A write returns no rows, so the
result-set oracle can't express it. `LiveDiffOracle::verify_write` instead compares
resulting **table state**: reset both engines to a seed, apply the original on MySQL and
the candidate on PG, then diff a state query across the two engines.

Proven on real MySQL 8.0 + PostgreSQL 16 (`tests/oracle_writes_test.rs`):

```
write-diff oracle: correct UPDATE/DELETE Equivalent; wrong write caught; untranslatable upsert honestly Errored.
write-diff oracle: sqlglot's UPDATE translation state-verified Equivalent.
```

- A correct `UPDATE` (`IFNULL`→`COALESCE`) and `DELETE` produce identical post-write
  state → `Equivalent`.
- A candidate that updates the *wrong row* → `Divergent` (state diff catches it).
- `INSERT … ON DUPLICATE KEY UPDATE` — which *no* deterministic generator translates
  (sqlglot 30.11.0 passes it through, PG rejects it) → `Error`, **honestly skipped**, an
  invalid upsert never accepted as if it worked.
- sqlglot's `UPDATE` translation, fed through the generator, is state-verified.

**Phase 2e — deep benchmark + user guide (built, 2026-06-15).** A broad 23-construct
corpus (`tests/fixtures/oracle/corpus_deep.toml`) run through every generator and verified
against **real MySQL** (`tests/oracle_deep_benchmark.rs`), producing a coverage matrix:

```
  per-generator behavioral coverage (oracle-verified Equivalent):
    regex      19/23      polyglot   21/23      sqlglot    23/23
    UNION      23/23   <- multi-pass (any generator verified)
```

The matrix is the honest picture: no deterministic tool is safe alone — `regex` *diverges*
on the string literal (silently wrong) and errors on GROUP_CONCAT/MOD/DATE_FORMAT;
`polyglot` *diverges* on `DATE_FORMAT` (keeps MySQL `%Y-%m` format codes — output that runs
on PG but returns the wrong string) and errors on `IF()`. Both divergences are caught
*only* by execution. The full operator guide — turning the feature on, every environment
variable, the container/sqlglot/LLM setup, running each benchmark, and the
capture→translate→replay→compare workflow — is in
**[`docs/oracle-verified-translation.md`](docs/oracle-verified-translation.md)**.

**Phase 2f — `oracle-replay` CLI command (built, 2026-06-15).** The proven library
pipeline is now a single subcommand. `src/transform/oracle/replay.rs::translate_profile`
runs every statement of a captured `.wkl` through the verified-search engine; the
`pg-retest oracle-replay` command (feature-gated) wires it to file I/O and an oracle
selector:

```bash
pg-retest oracle-replay --input workload.wkl --output translated.wkl --verify syntactic
# or --verify live  (PG_RETEST_ORACLE_URL + PG_RETEST_MYSQL_CMD)  for behavioral acceptance
```

It retains `original_sql` on every translated statement, sets the output's `source_dialect`
to `Postgres`, drops the unverifiable, and prints an `Oracle-Replay Report` (translated /
skipped, per-winning-generator counts, skip reasons). sqlglot/LLM join the cascade
automatically when configured. Proven end-to-end (capture → `oracle-replay` → `inspect`):
a 7-statement MySQL capture translated to `mysql_slow_log+oracle-translated`, and
`--verify live` without a MySQL configured fails fast with a clear message. The default
build is unaffected (the whole command is `#[cfg(feature = "polyglot-transform")]`).

**Phase 2g — Oracle SQL Trace capture + Oracle→PG (built, 2026-06-15).** Oracle is reached
by *upload*, not connection: `--source-type oracle-trace` (`capture::oracle_trace`) parses
an uploaded event-10046 `.trc` into a workload (`source_dialect = Oracle`; top-level
statements only, recursive dictionary SQL filtered). `SqlglotGenerator` is parameterized by
read dialect and `oracle-replay` is dialect-aware, so Oracle workloads translate via sqlglot
`read='oracle'`. **Proven end-to-end on real PG** (`scripts/e2e-replay.sh`, scenario D):
Oracle trace → capture → oracle-replay → replay, with `NVL→COALESCE` + INSERT/UPDATE landing
on the target. Anything sqlglot can't faithfully translate (e.g. `ROWNUM`) is behaviorally
rejected by `--verify live`, never shipped.

```
================ scripts/e2e-replay.sh:  9 passed, 0 failed ================
A PG→PG (csv)  B PG→PG (proxy)  C MySQL→PG  D Oracle→PG  E scale 3  F oracle-replay
```

**Phase 2h — Oracle bind-variable substitution (built, 2026-06-15).** The 10046 trace
parser now reads `BINDS` sections and substitutes bind placeholders positionally
(`WHERE id = :1` + `value=42` → `WHERE id = 42`; numbers pass through, strings become
quoted literals with `''` escaping). Proven in `scripts/e2e-replay.sh` scenario D: a bound
`UPDATE price = NVL(:1,0)+7` with `:1=100` resolves to `COALESCE(100,0)+7 = 107` on the
target PG — bind substitution + Oracle→PG translation + replay, end to end. Heuristic
(`:\w+`, not lexer-aware) so a `:NN` inside a string literal could be misread; the oracle
gates the result. So bind-heavy OLTP Oracle traces now replay faithfully.

**Phase 2i (next):** richer per-cell normalization (float/decimal tolerance, timezones),
transaction-aware skipping (FR-XFORM-8), an `ON CONFLICT` upsert via the LLM generator,
and an AWR/`V$SQL` extract capture source (the easy-to-produce Oracle alternative).
