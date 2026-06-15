//! Behavioral benchmark: every accepted translation is verified by EXECUTION on a real
//! PostgreSQL, not by parsing. Proves the multi-pass engine picks a behavior-preserving
//! candidate per query — and that the oracle rejects regex's mistranslations AND
//! polyglot's syntactically-valid-but-semantically-wrong passthroughs.
//!
//!   PG_RETEST_ORACLE_URL=... \
//!   cargo test --features polyglot-transform --test oracle_translation_benchmark -- --nocapture
//!
//! Skips cleanly when no PostgreSQL is reachable (CI without a DB stays green).
#![cfg(feature = "polyglot-transform")]

use std::collections::BTreeMap;

use pg_retest::transform::mysql_to_pg::mysql_to_pg_pipeline;
use pg_retest::transform::oracle::corpus::Corpus;
use pg_retest::transform::oracle::engine::{
    translate_verified, CandidateGenerator, PipelineGenerator,
};
use pg_retest::transform::oracle::golden::{conn_str, GoldenOracle};
use pg_retest::transform::polyglot::mysql_to_pg_polyglot_pipeline;

#[tokio::test]
async fn oracle_verified_translation_benchmark() {
    let oracle = match GoldenOracle::connect(&conn_str()).await {
        Ok(o) => o,
        Err(_) => {
            eprintln!("SKIP: PostgreSQL not available at {}", conn_str());
            return;
        }
    };
    oracle
        .batch(include_str!("fixtures/oracle/seed.sql"))
        .await
        .expect("seed should load");

    let corpus = Corpus::from_toml(include_str!("fixtures/oracle/corpus.toml")).unwrap();
    let generators: Vec<Box<dyn CandidateGenerator>> = vec![
        PipelineGenerator::boxed("polyglot", mysql_to_pg_polyglot_pipeline()),
        PipelineGenerator::boxed("regex", mysql_to_pg_pipeline()),
    ];

    let mut wins: BTreeMap<&str, usize> = BTreeMap::new();
    let mut skipped = 0usize;
    let mut winner_of: BTreeMap<String, Option<&str>> = BTreeMap::new();

    println!("\n  Oracle-verified MySQL → PostgreSQL translation (behavioral)\n");
    println!(
        "  {:<20} | {:<8} | winner   | runners-up verdicts",
        "case", "verified"
    );
    println!("  {}", "-".repeat(74));

    for case in &corpus.case {
        let r =
            translate_verified(&generators, &oracle, &case.mysql_sql, &case.correct_pg_sql).await;
        match r.winner {
            Some(w) => *wins.entry(w).or_default() += 1,
            None => skipped += 1,
        }
        winner_of.insert(case.id.clone(), r.winner);
        // Show non-winning attempts' verdicts so the oracle's rejections are visible.
        let runners: Vec<String> = r
            .attempts
            .iter()
            .filter(|a| Some(a.method) != r.winner)
            .map(|a| match &a.verdict {
                Some(v) => format!("{}={:?}", a.method, v),
                None => format!("{}=declined", a.method),
            })
            .collect();
        println!(
            "  {:<20} | {:<8} | {:<8} | {}",
            case.id,
            if r.winner.is_some() { "yes" } else { "NO" },
            r.winner.unwrap_or("—"),
            runners.join(", ")
        );
    }

    println!("\n  wins by method: {wins:?}   unverifiable/skipped: {skipped}");
    println!(
        "  (every 'verified' row was certified Equivalent by EXECUTION on PostgreSQL,\n   \
         not by parsing — behavior, not syntax.)\n"
    );

    // The multi-pass thesis, asserted: each engine is the right tool for DIFFERENT
    // queries, and the oracle picks correctly. polyglot wins the string-literal case
    // (regex corrupts it); regex wins the IF()-function case (polyglot's passthrough
    // parses but fails on execution — the behavioral oracle catches what the syntactic
    // gate could not).
    assert_eq!(
        winner_of.get("string_literal_if").copied().flatten(),
        Some("polyglot"),
        "polyglot must win the string-literal case (regex corrupts the literal)"
    );
    assert_eq!(
        winner_of.get("if_function").copied().flatten(),
        Some("regex"),
        "regex must win IF()->CASE (polyglot's IF() passthrough fails on execution)"
    );
    assert!(
        wins.get("polyglot").copied().unwrap_or(0) >= 1
            && wins.get("regex").copied().unwrap_or(0) >= 1,
        "both engines should win at least one query — that is the multi-pass value"
    );
}
