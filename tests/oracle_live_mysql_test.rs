//! Live differential oracle: run the ORIGINAL MySQL query on a real MySQL and the
//! candidate translation on a real PostgreSQL, then diff. The truest oracle — the actual
//! migration-validation primitive.
//!
//!   PG_RETEST_ORACLE_URL="host=localhost port=5441 user=oracle password=oracle dbname=oracle" \
//!   PG_RETEST_MYSQL_CMD="docker exec pgretest-oracle-mysql mysql -uroot -proot -N --batch --raw oracle" \
//!   cargo test --features polyglot-transform --test oracle_live_mysql_test -- --nocapture
//!
//! Skips cleanly unless BOTH PostgreSQL and a MySQL CLI are configured/reachable.
//!
//! NOTE: this binary resets a shared `oracle_users` fixture in both engines. When run
//! against a live PostgreSQL alongside other oracle DB-tests (e.g. the benchmark, which
//! also seeds `oracle_users`), run one binary at a time or with `--test-threads=1` to
//! avoid a concurrent-seed race. Without a DB every test skips, so CI is unaffected.
#![cfg(feature = "polyglot-transform")]

use std::collections::BTreeMap;

use pg_retest::transform::mysql_to_pg::mysql_to_pg_pipeline;
use pg_retest::transform::oracle::corpus::Corpus;
use pg_retest::transform::oracle::engine::{
    translate_verified, CandidateGenerator, PipelineGenerator,
};
use pg_retest::transform::oracle::golden::conn_str;
use pg_retest::transform::oracle::live::LiveDiffOracle;
use pg_retest::transform::oracle::{Oracle, Verdict};
use pg_retest::transform::polyglot::mysql_to_pg_polyglot_pipeline;

async fn setup() -> Option<LiveDiffOracle> {
    let argv = LiveDiffOracle::mysql_argv_from_env()?;
    let oracle = match LiveDiffOracle::connect(&conn_str(), argv).await {
        Ok(o) => o,
        Err(e) => {
            eprintln!("SKIP: PostgreSQL not reachable: {e}");
            return None;
        }
    };
    if oracle
        .batch_pg(include_str!("fixtures/oracle/seed.sql"))
        .await
        .is_err()
    {
        eprintln!("SKIP: could not seed PostgreSQL");
        return None;
    }
    if let Err(e) = oracle
        .mysql_exec(include_str!("fixtures/oracle/seed_mysql.sql"))
        .await
    {
        eprintln!("SKIP: MySQL CLI not usable: {e}");
        return None;
    }
    Some(oracle)
}

/// One test (seeds once) covering both claims: (1) every author-verified translation is
/// behaviorally Equivalent to its MySQL original on REAL engines and a wrong candidate is
/// caught; (2) the full multi-pass engine, refereed by the live oracle, picks the right
/// engine per query.
#[tokio::test]
async fn live_differential_oracle_end_to_end() {
    let Some(oracle) = setup().await else { return };
    let corpus = Corpus::from_toml(include_str!("fixtures/oracle/corpus.toml")).unwrap();

    // (1) Each correct PG translation must match its ORIGINAL MySQL query when both run
    // on REAL engines — proving the corpus AND the cross-engine oracle (NULL handling,
    // record/TSV normalization, ordering).
    for case in &corpus.case {
        let v = oracle.verify(&case.correct_pg_sql, &case.mysql_sql).await;
        assert_eq!(v, Verdict::Equivalent, "case `{}` diverged: {v:?}", case.id);
    }
    // A deliberately wrong candidate is caught against live MySQL.
    let headline = corpus
        .case
        .iter()
        .find(|c| c.id == "string_literal_if")
        .unwrap();
    let wrong = oracle
        .verify(
            "SELECT 'CASE WHEN x THEN 1 ELSE 0 END' AS lit",
            &headline.mysql_sql,
        )
        .await;
    assert!(
        matches!(wrong, Verdict::Divergent { .. }),
        "wrong candidate not caught against live MySQL: {wrong:?}"
    );
    eprintln!(
        "live-diff oracle: all {} corpus translations Equivalent vs real MySQL; wrong candidate caught.",
        corpus.case.len()
    );

    // (2) The full multi-pass engine, refereed by the live oracle (reference = the
    // original MySQL query), picks the right engine per query — same verdicts as the
    // golden benchmark, but now verified by EXECUTION on real MySQL vs real PostgreSQL.
    let generators: Vec<Box<dyn CandidateGenerator>> = vec![
        PipelineGenerator::boxed("polyglot", mysql_to_pg_polyglot_pipeline()),
        PipelineGenerator::boxed("regex", mysql_to_pg_pipeline()),
    ];
    let mut winner_of: BTreeMap<String, Option<&str>> = BTreeMap::new();
    for case in &corpus.case {
        let r = translate_verified(&generators, &oracle, &case.mysql_sql, &case.mysql_sql).await;
        winner_of.insert(case.id.clone(), r.winner);
    }
    assert_eq!(
        winner_of.get("string_literal_if").copied().flatten(),
        Some("polyglot"),
        "polyglot must win the string-literal case (regex corrupts it)"
    );
    assert_eq!(
        winner_of.get("if_function").copied().flatten(),
        Some("regex"),
        "regex must win IF()->CASE (polyglot's IF() fails on PG execution)"
    );
}
