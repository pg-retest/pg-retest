//! sqlglot generator adds oracle-verified coverage: it translates MySQL `IF()` →
//! `CASE`, the exact case the polyglot 0.5.4 port passes through (and PG rejects at
//! execution). In the verified-search cascade, polyglot declines and sqlglot wins.
//!
//!   PG_RETEST_ORACLE_URL="host=localhost port=5441 user=oracle password=oracle dbname=oracle" \
//!   PG_RETEST_SQLGLOT_PYTHON=/path/to/venv/bin/python \
//!   cargo test --features polyglot-transform --test oracle_sqlglot_test -- --nocapture
//!
//! Skips cleanly unless BOTH a PostgreSQL and a Python with `sqlglot` are available.
#![cfg(feature = "polyglot-transform")]

use pg_retest::transform::oracle::corpus::Corpus;
use pg_retest::transform::oracle::engine::{
    translate_verified, CandidateGenerator, PipelineGenerator,
};
use pg_retest::transform::oracle::golden::{conn_str, GoldenOracle};
use pg_retest::transform::oracle::sqlglot::SqlglotGenerator;
use pg_retest::transform::polyglot::mysql_to_pg_polyglot_pipeline;

#[tokio::test]
async fn sqlglot_generator_adds_oracle_verified_coverage() {
    let sg = SqlglotGenerator::from_env();
    if !sg.available().await {
        eprintln!(
            "SKIP: sqlglot not importable (set PG_RETEST_SQLGLOT_PYTHON to a python with sqlglot)"
        );
        return;
    }
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
    let if_case = corpus.case.iter().find(|c| c.id == "if_function").unwrap();

    // 1) sqlglot translates IF() → CASE (what polyglot passes through and PG rejects).
    let cand = sg
        .candidate(&if_case.mysql_sql)
        .await
        .expect("sqlglot should produce a candidate");
    assert!(
        cand.to_uppercase().contains("CASE"),
        "expected sqlglot to emit CASE, got: {cand}"
    );

    // 2) In the cascade [polyglot, sqlglot], polyglot declines (its IF() passthrough
    //    fails on PG EXECUTION) and sqlglot WINS — added, oracle-verified coverage.
    let generators: Vec<Box<dyn CandidateGenerator>> = vec![
        PipelineGenerator::boxed("polyglot", mysql_to_pg_polyglot_pipeline()),
        Box::new(SqlglotGenerator::from_env()),
    ];
    let r = translate_verified(
        &generators,
        &oracle,
        &if_case.mysql_sql,
        &if_case.correct_pg_sql,
    )
    .await;
    assert_eq!(
        r.winner,
        Some("sqlglot"),
        "sqlglot should win where polyglot fails; attempts: {:?}",
        r.attempts
    );
    eprintln!("sqlglot won `if_function` (polyglot declined: IF() fails on PG execution).");
}
