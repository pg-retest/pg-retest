//! DEEP behavioral benchmark: every candidate generator × a broad MySQL construct
//! corpus, each candidate verified by EXECUTION against a real MySQL (the live
//! differential oracle). Produces a per-construct × per-generator coverage matrix and
//! the multi-pass union coverage — the honest picture of which tool handles what.
//!
//!   PG_RETEST_ORACLE_URL="host=localhost port=5441 user=oracle password=oracle dbname=oracle" \
//!   PG_RETEST_MYSQL_CMD="docker exec pgretest-oracle-mysql mysql -uroot -proot -N --batch --raw oracle" \
//!   PG_RETEST_SQLGLOT_PYTHON=/path/to/venv/bin/python   # optional 3rd generator
//!   PG_RETEST_LLM_URL=http://localhost:11434/v1/chat/completions  # optional 4th
//!   cargo test --features polyglot-transform --test oracle_deep_benchmark -- --nocapture
//!
//! Skips cleanly unless BOTH PostgreSQL and a MySQL CLI are configured/reachable.
#![cfg(feature = "polyglot-transform")]

use std::collections::BTreeMap;

use pg_retest::transform::mysql_to_pg::mysql_to_pg_pipeline;
use pg_retest::transform::oracle::corpus::Corpus;
use pg_retest::transform::oracle::engine::{CandidateGenerator, PipelineGenerator};
use pg_retest::transform::oracle::golden::conn_str;
use pg_retest::transform::oracle::live::LiveDiffOracle;
use pg_retest::transform::oracle::llm::LlmGenerator;
use pg_retest::transform::oracle::sqlglot::SqlglotGenerator;
use pg_retest::transform::oracle::{Oracle, Verdict};
use pg_retest::transform::polyglot::mysql_to_pg_polyglot_pipeline;

async fn setup() -> Option<LiveDiffOracle> {
    let argv = LiveDiffOracle::mysql_argv_from_env()?;
    let oracle = LiveDiffOracle::connect(&conn_str(), argv).await.ok()?;
    if oracle
        .mysql_exec(include_str!("fixtures/oracle/seed_mysql.sql"))
        .await
        .is_err()
        || oracle
            .batch_pg(include_str!("fixtures/oracle/seed.sql"))
            .await
            .is_err()
    {
        eprintln!("SKIP: could not seed both engines");
        return None;
    }
    Some(oracle)
}

#[tokio::test]
async fn deep_benchmark_all_generators_vs_live_mysql() {
    let Some(oracle) = setup().await else {
        eprintln!("SKIP: need PG (PG_RETEST_ORACLE_URL) and MySQL (PG_RETEST_MYSQL_CMD)");
        return;
    };
    let corpus = Corpus::from_toml(include_str!("fixtures/oracle/corpus_deep.toml")).unwrap();

    // Build the generator fleet: deterministic always; sqlglot + LLM if configured.
    let mut generators: Vec<Box<dyn CandidateGenerator>> = vec![
        PipelineGenerator::boxed("regex", mysql_to_pg_pipeline()),
        PipelineGenerator::boxed("polyglot", mysql_to_pg_polyglot_pipeline()),
    ];
    if SqlglotGenerator::from_env().available().await {
        generators.push(Box::new(SqlglotGenerator::from_env()));
    }
    if let Some(llm) = LlmGenerator::from_env() {
        generators.push(Box::new(llm));
    }
    let names: Vec<&str> = generators.iter().map(|g| g.name()).collect();

    let mut equiv: BTreeMap<&str, usize> = BTreeMap::new();
    let mut union = 0usize;
    let mut uncovered: Vec<&str> = Vec::new();

    // Header.
    print!(
        "\n  DEEP benchmark — generator × construct, verified vs REAL MySQL\n\n  {:<26}",
        "construct"
    );
    for n in &names {
        print!(" | {n:<8}");
    }
    println!(" | multi-pass");
    println!("  {}", "-".repeat(26 + names.len() * 11 + 13));

    for case in &corpus.case {
        let mut cells: Vec<&str> = Vec::new();
        for g in &generators {
            let cell = match g.candidate(&case.mysql_sql).await {
                None => "decline",
                Some(c) => match oracle.verify(&c, &case.mysql_sql).await {
                    Verdict::Equivalent => "ok",
                    Verdict::Divergent { .. } => "DIVERGE",
                    Verdict::Error { .. } => "error",
                },
            };
            cells.push(cell);
        }
        let mut any = false;
        for (i, g) in generators.iter().enumerate() {
            if cells[i] == "ok" {
                *equiv.entry(g.name()).or_insert(0) += 1;
                any = true;
            }
        }
        if any {
            union += 1;
        } else {
            uncovered.push(&case.id);
        }
        print!("  {:<26}", case.note.chars().take(26).collect::<String>());
        for c in &cells {
            print!(" | {c:<8}");
        }
        println!(" | {}", if any { "yes" } else { "— NONE" });
    }

    let total = corpus.case.len();
    println!("\n  per-generator behavioral coverage (oracle-verified Equivalent):");
    for n in &names {
        let c = equiv.get(n).copied().unwrap_or(0);
        println!("    {n:<10} {c:>2}/{total}");
    }
    println!(
        "    {:<10} {union:>2}/{total}   <- multi-pass (any generator verified)",
        "UNION"
    );
    if !uncovered.is_empty() {
        println!("\n  uncovered by all deterministic generators (LLM territory): {uncovered:?}");
    }
    println!();

    // Assertions: the multi-pass engine covers at least as much as the best single tool
    // (and nearly everything), and every accepted translation was behavior-verified.
    let best_single = names
        .iter()
        .map(|n| equiv.get(n).copied().unwrap_or(0))
        .max()
        .unwrap_or(0);
    assert!(
        union >= best_single,
        "multi-pass union ({union}) must be >= the best single generator ({best_single})"
    );
    assert!(
        union >= total - 1,
        "multi-pass should cover (almost) every construct; uncovered: {uncovered:?}"
    );
    // No single deterministic generator handles the whole corpus alone — the diversity
    // is the point (e.g. regex mangles string literals; polyglot fails IF()-as-function).
    assert!(
        equiv.get("regex").copied().unwrap_or(0) < total,
        "regex alone should NOT cover everything (it mistranslates some constructs)"
    );
    assert!(
        equiv.get("polyglot").copied().unwrap_or(0) < total,
        "polyglot alone should NOT cover everything (e.g. IF()-as-function)"
    );
}
