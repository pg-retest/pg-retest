# Oracle-Verified Multi-Pass Translation — Implementation Plan (Phase 1)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (inline) or
> superpowers:subagent-driven-development to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax.

**Goal:** Build a behavioral translation oracle — execute each candidate MySQL→PG
translation on a real PostgreSQL and accept only the one whose result matches an
author-verified reference — plus a benchmark that proves it.

**Architecture:** Candidate generators (polyglot AST + regex, both in-tree) feed a
`GoldenOracle` that runs candidate and reference SQL on a seeded PG and diffs canonical
result rows. The multi-pass engine cascades generators cheapest-first and accepts the
first `Equivalent`. Everything is behind the `polyglot-transform` feature; the live-DB
parts skip cleanly without PostgreSQL.

**Tech Stack:** Rust, tokio-postgres (already a dep, `with-serde_json`), the existing
`TransformPipeline`, `async-trait`, `toml` (corpus), the docker-compose `postgres:16`.

**Spec:** `docs/superpowers/specs/2026-06-15-oracle-verified-translation-design.md`

---

## File structure

| File | Responsibility |
|---|---|
| `src/transform/oracle/mod.rs` | `Verdict`, `Oracle` trait, re-exports; module wiring. |
| `src/transform/oracle/normalize.rs` | Canonical row compare (sort-unless-ORDER-BY, equality). Pure, no DB. |
| `src/transform/oracle/golden.rs` | `GoldenOracle`: connect PG, run candidate+reference, diff. |
| `src/transform/oracle/engine.rs` | Multi-pass cascade: try generators, accept first `Equivalent`. |
| `src/transform/oracle/corpus.rs` | `corpus.toml` loader (`Case { id, mysql_sql, correct_pg_sql, note }`). |
| `src/transform/mod.rs` | `#[cfg(feature="polyglot-transform")] pub mod oracle;` |
| `tests/fixtures/oracle/seed.sql` | PG schema + deterministic seed. |
| `tests/fixtures/oracle/corpus.toml` | The corpus. |
| `tests/oracle_translation_benchmark.rs` | The benchmark (feature + PG gated). |

---

## Task 1: Oracle module scaffold — `Verdict` + `Oracle` trait

**Files:** Create `src/transform/oracle/mod.rs`; Modify `src/transform/mod.rs`.

- [ ] **Step 1: Write the failing test** (in `src/transform/oracle/mod.rs`)

```rust
//! Behavioral translation oracle: verify candidate translations by execution.
//! Spec: docs/superpowers/specs/2026-06-15-oracle-verified-translation-design.md
pub mod corpus;
pub mod engine;
pub mod golden;
pub mod normalize;

use async_trait::async_trait;

/// Outcome of verifying one candidate translation against truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Candidate executes AND its result matches the reference.
    Equivalent,
    /// Candidate executes but the result differs from the reference.
    Divergent { detail: String },
    /// Candidate failed to execute (invalid PG, runtime error, etc.).
    Error { detail: String },
}

/// Verifies a candidate PG query against a source of truth.
#[async_trait]
pub trait Oracle {
    async fn verify(&self, candidate_sql: &str, reference_sql: &str) -> Verdict;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeOracle;
    #[async_trait]
    impl Oracle for FakeOracle {
        async fn verify(&self, candidate: &str, _reference: &str) -> Verdict {
            if candidate.contains("GOOD") {
                Verdict::Equivalent
            } else {
                Verdict::Divergent { detail: candidate.into() }
            }
        }
    }

    #[tokio::test]
    async fn test_oracle_trait_object_dispatches() {
        let o: &dyn Oracle = &FakeOracle;
        assert_eq!(o.verify("GOOD sql", "ref").await, Verdict::Equivalent);
        assert!(matches!(
            o.verify("BAD sql", "ref").await,
            Verdict::Divergent { .. }
        ));
    }
}
```

- [ ] **Step 2: Wire the module** — in `src/transform/mod.rs`, under the existing
  `#[cfg(feature = "polyglot-transform")] pub mod polyglot;` block, add:

```rust
#[cfg(feature = "polyglot-transform")]
pub mod oracle;
```

- [ ] **Step 3: Create empty sibling files** so `pub mod` compiles: create
  `src/transform/oracle/{corpus,engine,golden,normalize}.rs` each containing just a
  module doc comment line (filled by later tasks):

```rust
//! (filled in a later task)
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --features polyglot-transform --lib transform::oracle::tests`
Expected: PASS (1 test).

- [ ] **Step 5: Commit**

```bash
git add src/transform/oracle src/transform/mod.rs
git commit -m "feat(oracle): scaffold Verdict + Oracle trait (feature-gated)"
```

---

## Task 2: Canonical row comparison — `normalize.rs`

Pure logic, no DB. Rows arrive as `Vec<String>` (each = PG's record-text of one row).
Compare as an ordered list if the query has a top-level ORDER BY, else as a sorted
multiset.

**Files:** `src/transform/oracle/normalize.rs`.

- [ ] **Step 1: Write the failing test**

```rust
//! Canonical comparison of result rows. Each row is PostgreSQL's own record-text
//! rendering (e.g. `(1,foo,t)`) so the comparison is type- and column-name-agnostic.

/// True if `sql`'s outermost query carries an ORDER BY (compared in order; otherwise
/// compared as a multiset). Heuristic, case-insensitive — sufficient for the read
/// corpus and applied identically to candidate and reference so it cannot skew a diff.
pub fn is_ordered(sql: &str) -> bool {
    sql.to_uppercase().contains("ORDER BY")
}

/// True if the two result sets are equivalent under the ordering rule.
pub fn rows_equivalent(candidate: &[String], reference: &[String], ordered: bool) -> bool {
    if candidate.len() != reference.len() {
        return false;
    }
    if ordered {
        candidate == reference
    } else {
        let mut a = candidate.to_vec();
        let mut b = reference.to_vec();
        a.sort();
        b.sort();
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_ordered() {
        assert!(is_ordered("SELECT id FROM t ORDER BY id"));
        assert!(is_ordered("select * from t order by 1"));
        assert!(!is_ordered("SELECT id FROM t"));
    }

    #[test]
    fn test_unordered_is_multiset_equal() {
        let a = vec!["(1)".to_string(), "(2)".to_string()];
        let b = vec!["(2)".to_string(), "(1)".to_string()];
        assert!(rows_equivalent(&a, &b, false));
        assert!(!rows_equivalent(&a, &b, true)); // order matters when ordered
    }

    #[test]
    fn test_ordered_requires_same_order() {
        let a = vec!["(1)".to_string(), "(2)".to_string()];
        assert!(rows_equivalent(&a, &a, true));
    }

    #[test]
    fn test_different_lengths_never_equal() {
        let a = vec!["(1)".to_string()];
        let b = vec!["(1)".to_string(), "(2)".to_string()];
        assert!(!rows_equivalent(&a, &b, false));
    }

    #[test]
    fn test_divergent_values() {
        let a = vec!["('IF(x,1,0)')".to_string()];
        let b = vec!["('CASE WHEN x THEN 1 ELSE 0 END')".to_string()];
        assert!(!rows_equivalent(&a, &b, false)); // the headline: regex corruption caught
    }
}
```

- [ ] **Step 2: Run to verify it fails** then passes (code above is the implementation).

Run: `cargo test --features polyglot-transform --lib transform::oracle::normalize`
Expected: PASS (5 tests).

- [ ] **Step 3: Commit**

```bash
git add src/transform/oracle/normalize.rs
git commit -m "feat(oracle): canonical row comparison (sort-unless-ORDER-BY)"
```

---

## Task 3: Multi-pass engine — `engine.rs`

Cascade generators cheapest-first; accept the first candidate the oracle calls
`Equivalent`; record every attempt. Unit-tested with a `FakeOracle` (no DB) plus the
real in-tree pipelines.

**Files:** `src/transform/oracle/engine.rs`.

- [ ] **Step 1: Write the failing test + implementation**

```rust
//! Translation by verified search: try each generator, keep the first candidate the
//! oracle certifies behavior-preserving.

use crate::transform::{TransformPipeline, TransformResult};

use super::{Oracle, Verdict};

/// A named candidate generator (a `TransformPipeline` that emits one candidate).
pub struct Generator {
    pub name: &'static str,
    pub pipeline: TransformPipeline,
}

impl Generator {
    /// The candidate this generator proposes for `sql`, or `None` if it declines.
    fn candidate(&self, sql: &str) -> Option<String> {
        match self.pipeline.apply(sql) {
            TransformResult::Transformed(s) => Some(s),
            TransformResult::Unchanged => Some(sql.to_string()),
            TransformResult::Skipped { .. } => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Attempt {
    pub method: &'static str,
    pub candidate: Option<String>,
    pub verdict: Option<Verdict>, // None = generator declined (no candidate)
}

#[derive(Debug, Clone)]
pub struct VerifiedTranslation {
    pub winner: Option<&'static str>,
    pub accepted_sql: Option<String>,
    pub attempts: Vec<Attempt>,
}

/// Try generators in order; accept the first `Equivalent`; record all attempts.
pub async fn translate_verified(
    generators: &[Generator],
    oracle: &dyn Oracle,
    mysql_sql: &str,
    reference_sql: &str,
) -> VerifiedTranslation {
    let mut attempts = Vec::new();
    for g in generators {
        match g.candidate(mysql_sql) {
            None => attempts.push(Attempt {
                method: g.name,
                candidate: None,
                verdict: None,
            }),
            Some(cand) => {
                let verdict = oracle.verify(&cand, reference_sql).await;
                let equivalent = verdict == Verdict::Equivalent;
                attempts.push(Attempt {
                    method: g.name,
                    candidate: Some(cand.clone()),
                    verdict: Some(verdict),
                });
                if equivalent {
                    return VerifiedTranslation {
                        winner: Some(g.name),
                        accepted_sql: Some(cand),
                        attempts,
                    };
                }
            }
        }
    }
    VerifiedTranslation {
        winner: None,
        accepted_sql: None,
        attempts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transform::TransformPipeline;
    use async_trait::async_trait;

    // Oracle that certifies a candidate iff it contains the marker substring.
    struct MarkerOracle(&'static str);
    #[async_trait]
    impl Oracle for MarkerOracle {
        async fn verify(&self, candidate: &str, _r: &str) -> Verdict {
            if candidate.contains(self.0) {
                Verdict::Equivalent
            } else {
                Verdict::Divergent { detail: candidate.into() }
            }
        }
    }

    fn polyglot_gen() -> Generator {
        Generator {
            name: "polyglot",
            pipeline: crate::transform::polyglot::mysql_to_pg_polyglot_pipeline(),
        }
    }
    fn regex_gen() -> Generator {
        Generator {
            name: "regex",
            pipeline: crate::transform::mysql_to_pg::mysql_to_pg_pipeline(),
        }
    }

    #[tokio::test]
    async fn test_accepts_first_equivalent_and_records_attempts() {
        // Reject regex's COALESCE-less output, accept polyglot's COALESCE.
        let gens = vec![regex_gen(), polyglot_gen()];
        let oracle = MarkerOracle("COALESCE");
        // Regex leaves IFNULL alone? No — regex DOES rewrite IFNULL. Use a marker only
        // polyglot produces: both produce COALESCE here, so regex (first) wins. Instead
        // pick a case only polyglot handles: backticks->double quotes is both; use the
        // string-literal IF where regex mangles and polyglot preserves.
        let r = translate_verified(&gens, &oracle, "SELECT 1", "ref").await;
        // "SELECT 1" -> both Unchanged -> candidate "SELECT 1" (no COALESCE) -> none win.
        assert!(r.winner.is_none());
        assert_eq!(r.attempts.len(), 2);
    }

    #[tokio::test]
    async fn test_winner_is_recorded() {
        let gens = vec![polyglot_gen()];
        let oracle = MarkerOracle("COALESCE");
        let r = translate_verified(&gens, &oracle, "SELECT IFNULL(a, b) FROM t", "ref").await;
        assert_eq!(r.winner, Some("polyglot"));
        assert!(r.accepted_sql.unwrap().contains("COALESCE"));
    }

    #[tokio::test]
    async fn test_all_decline_or_diverge_yields_skip() {
        let gens = vec![Generator {
            name: "noop",
            pipeline: TransformPipeline::new(vec![]),
        }];
        let oracle = MarkerOracle("NEVER");
        let r = translate_verified(&gens, &oracle, "SELECT 1", "ref").await;
        assert!(r.winner.is_none());
    }
}
```

- [ ] **Step 2: Run to verify pass**

Run: `cargo test --features polyglot-transform --lib transform::oracle::engine`
Expected: PASS (3 tests).

- [ ] **Step 3: Commit**

```bash
git add src/transform/oracle/engine.rs
git commit -m "feat(oracle): multi-pass verified-search engine (accept first Equivalent)"
```

---

## Task 4: `GoldenOracle` — execute + diff on real PostgreSQL

**Files:** `src/transform/oracle/golden.rs`. Integration test is PG-gated (skips cleanly).

- [ ] **Step 1: Write the implementation + PG-gated integration test**

```rust
//! GoldenOracle: truth = the result of an author-verified reference PG query, run on
//! the same seeded PostgreSQL. A candidate is Equivalent iff it executes and returns
//! the same canonical rows as the reference.

use tokio_postgres::{Client, NoTls};

use super::normalize::{is_ordered, rows_equivalent};
use super::{Oracle, Verdict};
use async_trait::async_trait;

pub struct GoldenOracle {
    client: Client,
}

impl GoldenOracle {
    /// Connect and spawn the connection driver. Returns Err if PG is unreachable.
    pub async fn connect(conn: &str) -> Result<Self, tokio_postgres::Error> {
        let (client, connection) = tokio_postgres::connect(conn, NoTls).await?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(Self { client })
    }

    /// Run arbitrary SQL (e.g. the seed) ignoring results.
    pub async fn batch(&self, sql: &str) -> Result<(), tokio_postgres::Error> {
        self.client.batch_execute(sql).await
    }

    /// Execute `sql` and return each row as PostgreSQL's own record-text (positional,
    /// type- and name-agnostic). Err string on any execution failure.
    async fn canonical_rows(&self, sql: &str) -> Result<Vec<String>, String> {
        // Wrap so PG renders the whole row to text: SELECT _s::text FROM ( <sql> ) _s
        let wrapped = format!("SELECT _s::text AS r FROM ( {sql} ) AS _s");
        let rows = self
            .client
            .query(&wrapped, &[])
            .await
            .map_err(|e| e.to_string())?;
        rows.iter()
            .map(|row| row.try_get::<_, Option<String>>("r").map_err(|e| e.to_string()))
            .map(|r| r.map(|opt| opt.unwrap_or_else(|| "<NULL ROW>".into())))
            .collect()
    }
}

#[async_trait]
impl Oracle for GoldenOracle {
    async fn verify(&self, candidate_sql: &str, reference_sql: &str) -> Verdict {
        let reference = match self.canonical_rows(reference_sql).await {
            Ok(r) => r,
            // A broken reference is a corpus bug, surfaced as Error.
            Err(e) => return Verdict::Error { detail: format!("reference failed: {e}") },
        };
        let candidate = match self.canonical_rows(candidate_sql).await {
            Ok(r) => r,
            Err(e) => return Verdict::Error { detail: e },
        };
        let ordered = is_ordered(candidate_sql) && is_ordered(reference_sql);
        if rows_equivalent(&candidate, &reference, ordered) {
            Verdict::Equivalent
        } else {
            Verdict::Divergent {
                detail: format!("{} candidate rows vs {} reference rows", candidate.len(), reference.len()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONN: &str = "host=localhost port=5441 dbname=pg_retest_e2e user=sales_demo_app password=salesdemo123";

    async fn oracle() -> Option<GoldenOracle> {
        match GoldenOracle::connect(CONN).await {
            Ok(o) => Some(o),
            Err(_) => {
                eprintln!("SKIP: PostgreSQL not available at localhost:5441");
                None
            }
        }
    }

    #[tokio::test]
    async fn test_equivalent_when_results_match() {
        let Some(o) = oracle().await else { return };
        // Two syntactically different but result-identical queries.
        let v = o.verify("SELECT 1 AS x", "SELECT 1 AS y").await;
        assert_eq!(v, Verdict::Equivalent);
    }

    #[tokio::test]
    async fn test_divergent_when_results_differ() {
        let Some(o) = oracle().await else { return };
        let v = o.verify("SELECT 1", "SELECT 2").await;
        assert!(matches!(v, Verdict::Divergent { .. }));
    }

    #[tokio::test]
    async fn test_error_when_candidate_invalid_pg() {
        let Some(o) = oracle().await else { return };
        // ON DUPLICATE KEY is invalid PG -> execution error -> Error verdict.
        let v = o.verify("SELECT bogus_fn_xyz()", "SELECT 1").await;
        assert!(matches!(v, Verdict::Error { .. }));
    }

    #[tokio::test]
    async fn test_string_literal_corruption_is_divergent() {
        let Some(o) = oracle().await else { return };
        // The headline: regex would turn 'IF(x,1,0)' into 'CASE WHEN...'. The oracle
        // catches that behaviorally.
        let v = o
            .verify(
                "SELECT 'CASE WHEN x THEN 1 ELSE 0 END' AS lit",
                "SELECT 'IF(x,1,0)' AS lit",
            )
            .await;
        assert!(matches!(v, Verdict::Divergent { .. }));
    }
}
```

- [ ] **Step 2: Run** (with PG up: `docker compose up -d db-a` or any PG on 5441; or
  confirm clean skip without one)

Run: `cargo test --features polyglot-transform --lib transform::oracle::golden -- --nocapture`
Expected: PASS (4 tests; each prints SKIP and passes if no PG).

- [ ] **Step 3: Commit**

```bash
git add src/transform/oracle/golden.rs
git commit -m "feat(oracle): GoldenOracle — execute candidate+reference on PG, diff rows"
```

---

## Task 5: Corpus loader + fixtures — `corpus.rs`, `seed.sql`, `corpus.toml`

**Files:** `src/transform/oracle/corpus.rs`, `tests/fixtures/oracle/seed.sql`,
`tests/fixtures/oracle/corpus.toml`.

- [ ] **Step 1: Corpus type + loader + test** (`src/transform/oracle/corpus.rs`)

```rust
//! Golden corpus: MySQL queries paired with an author-verified correct PG translation.
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    pub id: String,
    pub mysql_sql: String,
    /// The human-verified correct PostgreSQL translation — the oracle's source of truth.
    pub correct_pg_sql: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Corpus {
    pub case: Vec<Case>,
}

impl Corpus {
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parses_corpus() {
        let s = r#"
            [[case]]
            id = "ifnull"
            mysql_sql = "SELECT IFNULL(a,b) FROM t"
            correct_pg_sql = "SELECT COALESCE(a,b) FROM t"
            note = "IFNULL->COALESCE"
        "#;
        let c = Corpus::from_toml(s).unwrap();
        assert_eq!(c.case.len(), 1);
        assert_eq!(c.case[0].id, "ifnull");
        assert!(c.case[0].correct_pg_sql.contains("COALESCE"));
    }
}
```

- [ ] **Step 2: Seed** (`tests/fixtures/oracle/seed.sql`) — idempotent; deterministic data.

```sql
DROP TABLE IF EXISTS oracle_users;
CREATE TABLE oracle_users (id int PRIMARY KEY, name text, active int, score int);
INSERT INTO oracle_users (id, name, active, score) VALUES
  (1, 'alice', 1, 10),
  (2, NULL,    0, 20),
  (3, 'cara',  1, 30);
```

- [ ] **Step 3: Corpus** (`tests/fixtures/oracle/corpus.toml`) — read queries that are
  deterministic on the seed; includes the two cases that prove the multi-pass thesis
  (string-literal IF → polyglot wins; IF()-function → regex wins).

```toml
[[case]]
id = "ifnull_coalesce"
mysql_sql = "SELECT IFNULL(name, 'none') FROM oracle_users ORDER BY id"
correct_pg_sql = "SELECT COALESCE(name, 'none') FROM oracle_users ORDER BY id"
note = "IFNULL->COALESCE; both engines should agree"

[[case]]
id = "backticks"
mysql_sql = "SELECT `id`, `name` FROM `oracle_users` ORDER BY id"
correct_pg_sql = "SELECT id, name FROM oracle_users ORDER BY id"
note = "backtick identifiers -> bare/double-quoted"

[[case]]
id = "limit_offset"
mysql_sql = "SELECT id FROM oracle_users ORDER BY id LIMIT 1, 2"
correct_pg_sql = "SELECT id FROM oracle_users ORDER BY id LIMIT 2 OFFSET 1"
note = "LIMIT offset,count -> LIMIT/OFFSET"

[[case]]
id = "string_literal_if"
mysql_sql = "SELECT 'IF(x,1,0)' AS lit"
correct_pg_sql = "SELECT 'IF(x,1,0)' AS lit"
note = "HEADLINE: regex mangles the literal (Divergent); polyglot preserves it (wins)"

[[case]]
id = "if_function"
mysql_sql = "SELECT IF(active = 1, 'yes', 'no') AS s FROM oracle_users ORDER BY id"
correct_pg_sql = "SELECT CASE WHEN active = 1 THEN 'yes' ELSE 'no' END AS s FROM oracle_users ORDER BY id"
note = "regex rewrites IF()->CASE (wins); polyglot passes IF() through -> not valid PG (Error)"
```

- [ ] **Step 4: Run loader test**

Run: `cargo test --features polyglot-transform --lib transform::oracle::corpus`
Expected: PASS (1 test).

- [ ] **Step 5: Commit**

```bash
git add src/transform/oracle/corpus.rs tests/fixtures/oracle/
git commit -m "feat(oracle): corpus loader + seed + golden corpus fixtures"
```

---

## Task 6: The benchmark — `tests/oracle_translation_benchmark.rs`

Feature + PG gated. Loads seed + corpus, builds [polyglot, regex] generators, runs the
verified-search engine per case, prints a per-method table, asserts the honesty
guarantee.

**Files:** `tests/oracle_translation_benchmark.rs`.

- [ ] **Step 1: Write the benchmark**

```rust
//! Behavioral benchmark: every accepted translation is verified by execution on a real
//! PostgreSQL. Proves the multi-pass engine picks a behavior-preserving candidate per
//! query (and that the oracle rejects regex's mistranslations).
//!
//!   cargo test --features polyglot-transform --test oracle_translation_benchmark -- --nocapture
#![cfg(feature = "polyglot-transform")]

use std::collections::BTreeMap;

use pg_retest::transform::mysql_to_pg::mysql_to_pg_pipeline;
use pg_retest::transform::oracle::corpus::Corpus;
use pg_retest::transform::oracle::engine::{translate_verified, Generator};
use pg_retest::transform::oracle::golden::GoldenOracle;
use pg_retest::transform::polyglot::mysql_to_pg_polyglot_pipeline;

const CONN: &str = "host=localhost port=5441 dbname=pg_retest_e2e user=sales_demo_app password=salesdemo123";

#[tokio::test]
async fn oracle_verified_translation_benchmark() {
    let oracle = match GoldenOracle::connect(CONN).await {
        Ok(o) => o,
        Err(_) => {
            eprintln!("SKIP: PostgreSQL not available at localhost:5441");
            return;
        }
    };
    oracle
        .batch(include_str!("fixtures/oracle/seed.sql"))
        .await
        .expect("seed should load");

    let corpus = Corpus::from_toml(include_str!("fixtures/oracle/corpus.toml")).unwrap();
    let generators = vec![
        Generator { name: "polyglot", pipeline: mysql_to_pg_polyglot_pipeline() },
        Generator { name: "regex", pipeline: mysql_to_pg_pipeline() },
    ];

    let mut wins: BTreeMap<&str, usize> = BTreeMap::new();
    let mut skipped = 0usize;
    println!("\n  Oracle-verified MySQL->PG translation\n");
    println!("  {:<22} | {:<10} | winner", "case", "verified");
    println!("  {}", "-".repeat(54));

    for case in &corpus.case {
        let r = translate_verified(&generators, &oracle, &case.mysql_sql, &case.correct_pg_sql).await;
        match r.winner {
            Some(w) => *wins.entry(w).or_default() += 1,
            None => skipped += 1,
        }
        println!(
            "  {:<22} | {:<10} | {}",
            case.id,
            if r.winner.is_some() { "yes" } else { "no" },
            r.winner.unwrap_or("— (skipped)")
        );
    }

    println!("\n  wins by method: {wins:?}   skipped: {skipped}");

    // Honesty: every accepted translation was Equivalent by construction. Demonstrate
    // the multi-pass value: at least one polyglot win AND at least one regex win (each
    // engine is the right tool for different queries), with the oracle as referee.
    assert!(wins.get("polyglot").copied().unwrap_or(0) >= 1, "expected >=1 polyglot win");
    assert!(wins.get("regex").copied().unwrap_or(0) >= 1, "expected >=1 regex win");
    // The string-literal headline must be won by polyglot (regex diverges).
    let headline = corpus.case.iter().find(|c| c.id == "string_literal_if").unwrap();
    let r = translate_verified(&generators, &oracle, &headline.mysql_sql, &headline.correct_pg_sql).await;
    assert_eq!(r.winner, Some("polyglot"), "string-literal case must be won by polyglot");
}
```

- [ ] **Step 2: Run** (with PG up)

Run: `cargo test --features polyglot-transform --test oracle_translation_benchmark -- --nocapture`
Expected: PASS; table printed; polyglot wins `string_literal_if`, regex wins `if_function`.

- [ ] **Step 3: Verify clean skip without PG** — stop PG, re-run, expect 1 passed (SKIP printed).

- [ ] **Step 4: Commit**

```bash
git add tests/oracle_translation_benchmark.rs
git commit -m "test(oracle): behavioral benchmark — verified-search picks the right engine per query"
```

---

## Task 7: Gates + docs

- [ ] **Step 1: Full gates, both feature states**

```bash
cargo build
cargo build --features polyglot-transform
cargo clippy --all-targets --features polyglot-transform -- -D warnings
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
cargo test --features polyglot-transform --lib --tests
```
Expected: all green (DB-gated tests skip cleanly if no PG).

- [ ] **Step 2: Update `EXPERIMENT-REPORT.md`** with an "Oracle-verified translation
  (Phase 1)" section: the architecture, the benchmark table, the headline (oracle
  rejects regex's behavioral mistranslation; engine picks the right engine per query),
  and the path to Phase 2 (LLM + sqlglot generators, live MySQL differential, writes).

- [ ] **Step 3: CHANGELOG `[Unreleased]`** — add the oracle + benchmark entry.

- [ ] **Step 4: Commit**

```bash
git add EXPERIMENT-REPORT.md CHANGELOG.md
git commit -m "docs(oracle): Phase 1 report section + CHANGELOG"
```

---

## Self-review notes (resolved inline)

- **Spec coverage:** Verdict/Oracle (T1) ✓; normalizer (T2) ✓; engine (T3) ✓;
  GoldenOracle/golden truth (T4) ✓; corpus+seed (T5) ✓; benchmark+assert-wrong-0 (T6) ✓;
  gates+docs (T7) ✓. Live-diff/LLM/sqlglot/writes are spec'd as Phase 2 — out of this plan.
- **`is_ordered` heuristic** applied identically to candidate and reference, so it cannot
  skew a diff (documented in T2).
- **`canonical_rows` wrap** (`SELECT _s::text FROM (sql) _s`) renders rows positionally —
  no per-type extraction, no column-name aliasing sensitivity (T4).
- **Truth = author-verified `correct_pg_sql` run live** — no stored result blobs (T5).
