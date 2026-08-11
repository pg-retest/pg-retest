//! Behavioral translation oracle: verify candidate MySQL→PostgreSQL translations by
//! *executing* them on a real PostgreSQL and comparing results — not by parsing.
//!
//! This upgrades the transformer's guarantee from syntactic ("PG parses the output")
//! to behavioral ("PG runs the output and it returns the right rows"). The oracle is
//! what makes every candidate generator — even the regex rules that currently
//! mistranslate, even a future nondeterministic LLM — safe: a wrong candidate is
//! rejected, never trusted.
//!
//! Spec:  docs/superpowers/specs/2026-06-15-oracle-verified-translation-design.md
//! Plan:  docs/superpowers/plans/2026-06-15-oracle-verified-translation.md
//!
//! Phase 1: `GoldenOracle` (truth = an author-verified reference PG query run live),
//! polyglot + regex candidate generators, SELECT/read queries. Live-MySQL differential,
//! LLM/sqlglot generators, and writes are Phase 2.

pub mod corpus;
pub mod engine;
pub mod golden;
pub mod live;
pub mod llm;
pub mod normalize;
pub mod replay;
pub mod sqlglot;

use async_trait::async_trait;

/// Outcome of verifying one candidate translation against a source of truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Candidate executes AND its result matches the reference.
    Equivalent,
    /// Candidate executes but the result differs from the reference.
    Divergent { detail: String },
    /// Candidate failed to execute (invalid PG, runtime error, etc.).
    Error { detail: String },
}

/// Verifies a candidate PostgreSQL query against a reference source of truth.
#[async_trait]
pub trait Oracle {
    async fn verify(&self, candidate_sql: &str, reference_sql: &str) -> Verdict;
}

/// The weakest oracle: accept any candidate PostgreSQL's own parser accepts (the
/// transpiler's `pg_query` gate, lifted to the engine). Needs no database, so it lets the
/// multi-pass engine run anywhere — but it proves only *syntactic* validity, never
/// behavior. Use a `GoldenOracle`/`LiveDiffOracle` for a real migration decision.
pub struct SyntacticOracle;

#[async_trait]
impl Oracle for SyntacticOracle {
    async fn verify(&self, candidate_sql: &str, _reference_sql: &str) -> Verdict {
        if super::is_valid_postgres(candidate_sql) {
            Verdict::Equivalent
        } else {
            Verdict::Error {
                detail: "not valid PostgreSQL".into(),
            }
        }
    }
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
                Verdict::Divergent {
                    detail: candidate.into(),
                }
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
