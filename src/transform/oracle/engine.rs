//! Translation by verified search: try each candidate generator cheapest-first, and
//! keep the first candidate the oracle certifies behavior-preserving. Record every
//! attempt for an auditable trail.

use crate::transform::{TransformPipeline, TransformResult};

use super::{Oracle, Verdict};

/// A named candidate generator — a `TransformPipeline` that proposes one candidate.
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
    generators: &[Generator],
    oracle: &dyn Oracle,
    mysql_sql: &str,
    reference_sql: &str,
) -> VerifiedTranslation {
    let mut attempts = Vec::new();
    for g in generators {
        let Some(cand) = g.candidate(mysql_sql) else {
            attempts.push(Attempt {
                method: g.name,
                candidate: None,
                verdict: None,
            });
            continue;
        };
        let verdict = oracle.verify(&cand, reference_sql).await;
        let accepted = verdict == Verdict::Equivalent;
        attempts.push(Attempt {
            method: g.name,
            candidate: Some(cand.clone()),
            verdict: Some(verdict),
        });
        if accepted {
            return VerifiedTranslation {
                winner: Some(g.name),
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
    use async_trait::async_trait;

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
        // First generator declines (empty pipeline → Unchanged → candidate == input,
        // which lacks the marker → Divergent), second generator's candidate matches.
        let gens = vec![
            Generator {
                name: "noop",
                pipeline: TransformPipeline::new(vec![]),
            },
            polyglot_gen(),
        ];
        let oracle = MarkerOracle("COALESCE");
        let r = translate_verified(&gens, &oracle, "SELECT IFNULL(a, b) FROM t", "ref").await;
        assert_eq!(r.winner, Some("polyglot"));
        assert_eq!(r.attempts.len(), 2);
        // The first attempt was made and rejected (Divergent), not skipped silently.
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
}
