# Design — Oracle-Verified Multi-Pass SQL Translation

**Date:** 2026-06-15
**Branch:** `experimental/polyglot-transform` (continuation of the polyglot experiment)
**Status:** Approved design (pending spec review) → foundation build.

---

## 1. Thesis

pg-retest's translation runs **offline** (capture → transform → replay), never on a
real-time path. So we can spend compute to be **right** instead of fast. Translation
becomes a **verified search**: generate candidate PostgreSQL translations from several
methods, and rather than trusting any of them on *syntax*, **verify each candidate's
behavior** by executing it on a real PostgreSQL and diffing its result against known
truth. Accept only what is provably behavior-preserving; honestly skip the rest.

This upgrades the shipped transformer's guarantee from *"PostgreSQL parses the output"*
(syntactic) to *"PostgreSQL runs the output and it returns the right rows"*
(behavioral). The **benchmark** is the proof: a corpus run end-to-end through the
oracle, reporting per-method how many statements are behaviorally correct / skipped /
wrong (target: wrong = 0).

## 2. Architecture

```
 captured stmt ─► [ candidate generators, cheap → expensive ]
                    1. polyglot AST   2. regex   (Phase 2: 3. sqlglot  4. LLM)
                        │ each emits a candidate PG string
                        ▼
                 ┌─────────────────────────────┐
                 │   DIFFERENTIAL ORACLE        │
                 │   execute candidate on PG    │
                 │   normalize + diff vs truth  │
                 │   → Equivalent | Divergent |  │
                 │     Error                    │
                 └───────────────┬─────────────┘
        first Equivalent         │ none Equivalent
              ▼                   ▼
   accept + record method    honestly SKIP  ─────► benchmark tally
```

Per statement: try generators cheapest-first; the **first candidate the oracle
certifies `Equivalent` wins** (record which method). If none pass, skip. The oracle is
what makes every generator — even the regex rules that currently mistranslate, even a
nondeterministic LLM — *safe*: a wrong candidate is rejected, never trusted.

## 3. Locked decisions

| Decision | Choice |
|---|---|
| Oracle meaning | **Oracle = the verifier** (replay-and-diff refereeing translation). |
| Ground truth | **Hybrid:** golden-result corpus by default (reproducible, PG-only at test time); **live differential** (MySQL container, runtime diff) as an opt-in Phase-2 mode. |
| First build | **Oracle + behavioral benchmark**, candidates = **polyglot AST + regex** (both in-tree), **SELECT/read queries only**. |
| Generators (full) | polyglot AST, regex, sqlglot (python subprocess), LLM (existing multi-provider infra) — last two are Phase 2. |
| Feature gate | reuse `polyglot-transform` (uses `PolyglotTransformer`); live-DB execution env-gated like `replay_e2e` (skips cleanly with no PG). |

## 4. Components & interfaces (Phase 1)

All new code lives under `src/transform/oracle/` (feature-gated `polyglot-transform`).

### 4.1 Candidate generator
A generator is a **named `TransformPipeline`** — `mysql_to_pg_polyglot_pipeline()` and
`mysql_to_pg_pipeline()` (regex) already exist and both produce exactly one candidate
via `.apply(sql)`: `Transformed(sql)` → that candidate; `Unchanged` → the input itself
is the candidate; `Skipped` → this generator declines. Modeled as
`struct Generator { name: &str, pipeline: TransformPipeline }`. No trait change; no new
generator abstraction beyond this thin wrapper.

### 4.2 Differential oracle
```rust
pub enum Verdict {
    Equivalent,                       // executes on PG AND rows match truth
    Divergent { detail: String },     // executes but rows differ
    Error { detail: String },         // failed to execute (or invalid PG)
}
pub trait ResultOracle {
    // Verify a candidate PG query against recorded golden rows for `case_id`.
    async fn verify(&self, candidate_sql: &str, case_id: &str) -> Verdict;
}
```
- **`GoldenOracle`** (Phase 1): connects to a real PG (env `PG_RETEST_ORACLE_URL`,
  default `host=localhost port=5441 …`), runs the candidate, normalizes the result set,
  compares to the corpus's recorded golden rows. Skips cleanly if PG unreachable.
- **`LiveDiffOracle`** (Phase 2): runs the *original* on MySQL and the *candidate* on PG
  and diffs — same `Verdict`, different truth source.

### 4.3 Result-set normalization (the crux)
A canonical, engine-agnostic row form so trivially-different encodings don't read as
divergence:
- each cell → canonical string: `NULL` sentinel; bool→`t`/`f`; numeric/`decimal` to a
  fixed string form; float rounded to N decimals (configurable tolerance); bytea→hex;
  timestamps to ISO-8601 UTC; text verbatim.
- **row order**: if the query has a top-level `ORDER BY`, preserve order; otherwise sort
  rows by their canonical tuple before comparing (set-equality).
- column count/labels compared structurally; label *names* not required to match
  (translation may alias differently) unless the corpus pins them.
This normalizer is the riskiest unit and gets its own focused unit tests.

### 4.4 Golden corpus
`tests/fixtures/oracle/` :
- `seed.sql` — PostgreSQL schema + deterministic seed data the corpus queries run
  against.
- `corpus.toml` — entries `{ id, mysql_sql, expected_rows = [[...]], note }`. Each
  `expected_rows` is the canonical result of the **known-correct** PG translation on the
  seed (author-verified; documented as such). Phase-2 live-diff replaces this implicit
  MySQL truth with real MySQL execution.

### 4.5 Multi-pass engine
```rust
pub struct VerifiedTranslation {
    pub case_id: String,
    pub winner: Option<String>,   // method name, e.g. "polyglot" / "regex"; None = skipped
    pub accepted_sql: Option<String>,
    pub attempts: Vec<(String /*method*/, Verdict)>,  // full audit trail
}
```
Cascade generators in cost order; return the first `Equivalent`; record every attempt.

### 4.6 Benchmark (the proof)
`tests/oracle_translation_benchmark.rs` (feature-gated, PG-env-gated). Runs the corpus
through the engine and prints:
- per-method: certified-correct / declined / divergent-or-error counts;
- overall coverage (% of corpus behaviorally translated) and which method won each;
- **asserts wrong (accepted-but-divergent) == 0** — acceptance requires an `Equivalent`
  verdict, so this holds by construction; the assert is the regression tripwire.

## 5. Testing

- **Unit (no DB):** normalizer (ordering, NULL, numeric/bool/float tolerance), verdict
  classification, engine cascade/accept-first/skip logic (with a fake in-memory oracle).
- **Integration (PG-gated):** `GoldenOracle` against the seeded docker PG; the benchmark.
  All skip cleanly (`require_pg()` pattern) when no PG — CI without a DB stays green.
- Gates unchanged: `cargo test`, `clippy -D warnings`, `fmt --check`; feature OFF build
  untouched.

## 6. Out of scope (Phase 1) — explicit

- LLM and sqlglot-subprocess generators (Phase 2; sqlglot adds a python runtime dep,
  pre-approved).
- Writes/DML oracle (state comparison, not result-set) — Phase 2.
- Live MySQL differential mode — Phase 2.
- Full capture→translate→replay→compare CLI/web E2E — Phase 3.
- No `SqlTransformer` trait change; no `.wkl` version bump.

## 7. Risks

| Risk | Mitigation |
|---|---|
| Result normalization false-diffs (numeric/float/timezone) | configurable tolerance; focused unit tests; canonical forms; set-vs-ordered by `ORDER BY` presence. |
| Golden truth is author-asserted, not live MySQL | documented honestly; Phase-2 live-diff is the gold standard; corpus kept small + reviewed. |
| Benchmark needs a real PG | env-gated, skips cleanly; uses existing docker-compose `postgres:16`. |
| Non-determinism (later: LLM) | oracle rejects wrong candidates; cache LLM results; cost-cascade avoids needless calls. |
| Scope creep | Phase 1 is reads + 2 in-tree generators only; everything else explicitly deferred. |
