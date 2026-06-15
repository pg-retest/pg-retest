//! Writes/DML oracle: verify INSERT/UPDATE/DELETE translations by resulting TABLE STATE
//! (a write returns no rows, so result-set comparison can't express it). Reset both
//! engines, apply the original on MySQL and the candidate on PG, diff the table state.
//!
//!   PG_RETEST_ORACLE_URL="host=localhost port=5441 user=oracle password=oracle dbname=oracle" \
//!   PG_RETEST_MYSQL_CMD="docker exec pgretest-oracle-mysql mysql -uroot -proot -N --batch --raw oracle" \
//!   cargo test --features polyglot-transform --test oracle_writes_test -- --nocapture
//!
//! Skips cleanly unless BOTH PostgreSQL and a MySQL CLI are configured/reachable.
#![cfg(feature = "polyglot-transform")]

use pg_retest::transform::oracle::engine::CandidateGenerator;
use pg_retest::transform::oracle::golden::conn_str;
use pg_retest::transform::oracle::live::LiveDiffOracle;
use pg_retest::transform::oracle::sqlglot::SqlglotGenerator;
use pg_retest::transform::oracle::Verdict;

const RESET_PG: &str = include_str!("fixtures/oracle/seed.sql");
const RESET_MYSQL: &str = include_str!("fixtures/oracle/seed_mysql.sql");
const STATE: &str = "SELECT id, name, active FROM oracle_users ORDER BY id";

async fn setup() -> Option<LiveDiffOracle> {
    let argv = LiveDiffOracle::mysql_argv_from_env()?;
    let oracle = LiveDiffOracle::connect(&conn_str(), argv).await.ok()?;
    // Connectivity/seed sanity (skip if either engine isn't usable).
    if oracle.mysql_exec(RESET_MYSQL).await.is_err() {
        eprintln!("SKIP: MySQL CLI not usable");
        return None;
    }
    if oracle.batch_pg(RESET_PG).await.is_err() {
        eprintln!("SKIP: PostgreSQL not usable");
        return None;
    }
    Some(oracle)
}

#[tokio::test]
async fn write_diff_oracle_verifies_dml_by_table_state() {
    let Some(oracle) = setup().await else {
        eprintln!("SKIP: need PG (PG_RETEST_ORACLE_URL) and MySQL (PG_RETEST_MYSQL_CMD)");
        return;
    };

    // 1) Correct UPDATE translation (IFNULL → COALESCE): identical resulting state.
    let v = oracle
        .verify_write(
            "UPDATE oracle_users SET name = COALESCE(name, 'filled') WHERE id = 2",
            "UPDATE oracle_users SET name = IFNULL(name, 'filled') WHERE id = 2",
            RESET_PG,
            RESET_MYSQL,
            STATE,
        )
        .await;
    assert_eq!(v, Verdict::Equivalent, "correct UPDATE diverged: {v:?}");

    // 2) Correct DELETE: identical statement, identical state.
    let v = oracle
        .verify_write(
            "DELETE FROM oracle_users WHERE active = 0",
            "DELETE FROM oracle_users WHERE active = 0",
            RESET_PG,
            RESET_MYSQL,
            STATE,
        )
        .await;
    assert_eq!(v, Verdict::Equivalent, "correct DELETE diverged: {v:?}");

    // 3) A WRONG candidate (updates the wrong row) is caught by the state diff.
    let v = oracle
        .verify_write(
            "UPDATE oracle_users SET active = 5 WHERE id = 3", // wrong row
            "UPDATE oracle_users SET active = 5 WHERE id = 1", // original
            RESET_PG,
            RESET_MYSQL,
            STATE,
        )
        .await;
    assert!(
        matches!(v, Verdict::Divergent { .. }),
        "wrong write not caught: {v:?}"
    );

    // 4) ON DUPLICATE KEY UPDATE — no deterministic generator translates it, so the raw
    //    passthrough is invalid PG. The writes oracle Errors (honest skip), never
    //    accepting a broken upsert as if it worked.
    let updup = "INSERT INTO oracle_users (id, name, active) VALUES (1, 'upd', 9) ON DUPLICATE KEY UPDATE name = VALUES(name)";
    let v = oracle
        .verify_write(updup, updup, RESET_PG, RESET_MYSQL, STATE)
        .await;
    assert!(
        matches!(v, Verdict::Error { .. }),
        "untranslatable upsert candidate should Error, got: {v:?}"
    );

    eprintln!("write-diff oracle: correct UPDATE/DELETE Equivalent; wrong write caught; untranslatable upsert honestly Errored.");

    // 5) Bonus: if sqlglot is available, its UPDATE translation is state-verified.
    let sg = SqlglotGenerator::from_env();
    if sg.available().await {
        let mysql_upd = "UPDATE oracle_users SET name = IFNULL(name, 'sg') WHERE id = 2";
        let cand = sg
            .candidate(mysql_upd)
            .await
            .expect("sqlglot should translate the UPDATE");
        let v = oracle
            .verify_write(&cand, mysql_upd, RESET_PG, RESET_MYSQL, STATE)
            .await;
        assert_eq!(
            v,
            Verdict::Equivalent,
            "sqlglot UPDATE candidate `{cand}` diverged: {v:?}"
        );
        eprintln!("write-diff oracle: sqlglot's UPDATE translation state-verified Equivalent.");
    }
}
