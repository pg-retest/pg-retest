//! Translation by verified search: try each candidate generator cheapest-first, and
//! keep the first candidate the oracle certifies behavior-preserving. Record every
//! attempt for an auditable trail.
//!
//! Generators are heterogeneous and async: a deterministic `TransformPipeline`
//! (polyglot, regex) and an external/nondeterministic generator (the LLM, a subprocess
//! transpiler) implement the same `CandidateGenerator` trait. The oracle is what makes
//! every one of them safe — a wrong candidate is rejected, never trusted — so a flaky
//! LLM is no more dangerous than a buggy regex rule.

use async_trait::async_trait;

use crate::transform::{TransformPipeline, TransformResult};

use super::{Oracle, Verdict};

/// A named source of candidate PostgreSQL translations.
#[async_trait]
pub trait CandidateGenerator: Send + Sync {
    fn name(&self) -> &'static str;
    /// The candidate this generator proposes for `mysql_sql`, or `None` if it declines.
    async fn candidate(&self, mysql_sql: &str) -> Option<String>;
}

/// Adapts a deterministic `TransformPipeline` (polyglot, regex) to the async generator
/// API. `Transformed` → that candidate; `Unchanged` → the input itself; `Skipped` →
/// declines.
pub struct PipelineGenerator {
    name: &'static str,
    pipeline: TransformPipeline,
}

impl PipelineGenerator {
    pub fn new(name: &'static str, pipeline: TransformPipeline) -> Self {
        Self { name, pipeline }
    }

    pub fn boxed(name: &'static str, pipeline: TransformPipeline) -> Box<dyn CandidateGenerator> {
        Box::new(Self::new(name, pipeline))
    }
}

#[async_trait]
impl CandidateGenerator for PipelineGenerator {
    fn name(&self) -> &'static str {
        self.name
    }
    async fn candidate(&self, mysql_sql: &str) -> Option<String> {
        match self.pipeline.apply(mysql_sql) {
            TransformResult::Transformed(s) => Some(s),
            TransformResult::Unchanged => Some(mysql_sql.to_string()),
            TransformResult::Skipped { .. } => None,
        }
    }
}

/// One generator's attempt at a statement.
#[derive(Debug, Clone)]
pub struct Attempt {
    pub method: &'static str,
    /// The candidate SQL, or `None` if the generator declined to produce one.
    pub candidate: Option<String>,
    /// The oracle's verdict, or `None` if there was no candidate to verify.
    pub verdict: Option<Verdict>,
}

/// The result of verified-search translation for one statement.
#[derive(Debug, Clone)]
pub struct VerifiedTranslation {
    pub winner: Option<&'static str>,
    pub accepted_sql: Option<String>,
    pub attempts: Vec<Attempt>,
}

/// Try generators in order; accept the first `Equivalent`; record all attempts.
pub async fn translate_verified(
    generators: &[Box<dyn CandidateGenerator>],
    oracle: &dyn Oracle,
    mysql_sql: &str,
    reference_sql: &str,
) -> VerifiedTranslation {
    let mut attempts = Vec::new();
    for g in generators {
        let Some(cand) = g.candidate(mysql_sql).await else {
            attempts.push(Attempt {
                method: g.name(),
                candidate: None,
                verdict: None,
            });
            continue;
        };
        let verdict = oracle.verify(&cand, reference_sql).await;
        let accepted = verdict == Verdict::Equivalent;
        attempts.push(Attempt {
            method: g.name(),
            candidate: Some(cand.clone()),
            verdict: Some(verdict),
        });
        if accepted {
            return VerifiedTranslation {
                winner: Some(g.name()),
                accepted_sql: Some(cand),
                attempts,
            };
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

    /// Oracle that certifies a candidate iff it contains the marker substring.
    struct MarkerOracle(&'static str);
    #[async_trait]
    impl Oracle for MarkerOracle {
        async fn verify(&self, candidate: &str, _r: &str) -> Verdict {
            if candidate.contains(self.0) {
                Verdict::Equivalent
            } else {
                Verdict::Divergent {
                    detail: candidate.into(),
                }
            }
        }
    }

    /// A generator that always proposes a fixed candidate — stands in for any external
    /// or nondeterministic tool (e.g. a flaky LLM) for the safety proof.
    struct StubGenerator(&'static str, &'static str); // (name, fixed candidate)
    #[async_trait]
    impl CandidateGenerator for StubGenerator {
        fn name(&self) -> &'static str {
            self.0
        }
        async fn candidate(&self, _sql: &str) -> Option<String> {
            Some(self.1.to_string())
        }
    }

    fn polyglot_gen() -> Box<dyn CandidateGenerator> {
        PipelineGenerator::boxed(
            "polyglot",
            crate::transform::polyglot::mysql_to_pg_polyglot_pipeline(),
        )
    }
    fn regex_gen() -> Box<dyn CandidateGenerator> {
        PipelineGenerator::boxed(
            "regex",
            crate::transform::mysql_to_pg::mysql_to_pg_pipeline(),
        )
    }

    #[tokio::test]
    async fn test_winner_recorded_and_sql_accepted() {
        let gens = vec![polyglot_gen()];
        let oracle = MarkerOracle("COALESCE");
        let r = translate_verified(&gens, &oracle, "SELECT IFNULL(a, b) FROM t", "ref").await;
        assert_eq!(r.winner, Some("polyglot"));
        assert!(r.accepted_sql.unwrap().contains("COALESCE"));
        assert_eq!(r.attempts.len(), 1);
    }

    #[tokio::test]
    async fn test_cascade_skips_declining_generator_and_accepts_next() {
        let gens = vec![
            PipelineGenerator::boxed("noop", TransformPipeline::new(vec![])),
            polyglot_gen(),
        ];
        let oracle = MarkerOracle("COALESCE");
        let r = translate_verified(&gens, &oracle, "SELECT IFNULL(a, b) FROM t", "ref").await;
        assert_eq!(r.winner, Some("polyglot"));
        assert_eq!(r.attempts.len(), 2);
        assert_eq!(r.attempts[0].method, "noop");
        assert!(matches!(
            r.attempts[0].verdict,
            Some(Verdict::Divergent { .. })
        ));
    }

    #[tokio::test]
    async fn test_all_diverge_yields_no_winner() {
        let gens = vec![regex_gen(), polyglot_gen()];
        let oracle = MarkerOracle("NEVER_MATCHES");
        let r = translate_verified(&gens, &oracle, "SELECT IFNULL(a, b) FROM t", "ref").await;
        assert!(r.winner.is_none());
        assert!(r.accepted_sql.is_none());
        assert_eq!(r.attempts.len(), 2);
    }

    /// THE SAFETY PROOF: a generator that emits WRONG SQL (a flaky external tool) is
    /// caught by the oracle and rejected; the engine recovers with the next generator.
    /// Wrong output is never trusted — this is what makes a nondeterministic LLM safe.
    #[tokio::test]
    async fn test_flaky_generator_is_rejected_engine_recovers() {
        let gens = vec![
            Box::new(StubGenerator("flaky_llm", "SELECT wrong_garbage"))
                as Box<dyn CandidateGenerator>,
            polyglot_gen(),
        ];
        let oracle = MarkerOracle("COALESCE");
        let r = translate_verified(&gens, &oracle, "SELECT IFNULL(a, b) FROM t", "ref").await;
        // The flaky generator's wrong candidate was REJECTED, not accepted...
        assert_eq!(r.attempts[0].method, "flaky_llm");
        assert!(matches!(
            r.attempts[0].verdict,
            Some(Verdict::Divergent { .. })
        ));
        // ...and the engine recovered with a verified-correct candidate.
        assert_eq!(r.winner, Some("polyglot"));
    }

    /// And when the external tool IS right, it wins — the oracle accepts it.
    #[tokio::test]
    async fn test_correct_external_generator_wins() {
        let gens = vec![
            Box::new(StubGenerator("good_llm", "SELECT COALESCE(a, b) FROM t"))
                as Box<dyn CandidateGenerator>,
            polyglot_gen(),
        ];
        let oracle = MarkerOracle("COALESCE");
        let r = translate_verified(&gens, &oracle, "SELECT IFNULL(a, b) FROM t", "ref").await;
        assert_eq!(r.winner, Some("good_llm"));
        assert_eq!(r.attempts.len(), 1); // accepted first, no fall-through
    }
}
