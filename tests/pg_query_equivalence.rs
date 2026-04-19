//! Equivalence harness: for every SQL in the corpus, assert that
//! `correlate::capture::has_returning` (our wrapper) returns the same value
//! as a direct pg_query AST walk (the oracle). Divergence = test failure.
//!
//! Phase 3 will extend this to cover extract_tables and
//! extract_filter_columns with their own oracles against the same corpus.
//!
//! Part of SC-008.

use pg_query::NodeEnum;
use pg_retest::correlate::capture::has_returning as subject_has_returning;
use std::fs;
use std::path::PathBuf;

fn corpus() -> Vec<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sql_corpus.txt");
    let text = fs::read_to_string(path).expect("corpus file");
    text.lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|s| s.to_string())
        .collect()
}

/// Oracle: walk pg_query's AST directly.
fn oracle_has_returning(sql: &str) -> Result<bool, String> {
    let parsed = pg_query::parse(sql).map_err(|e| format!("{}", e))?;
    for stmt in parsed.protobuf.stmts.iter() {
        if let Some(node) = stmt.stmt.as_ref().and_then(|s| s.node.as_ref()) {
            if walk(node)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn walk(node: &NodeEnum) -> Result<bool, String> {
    match node {
        NodeEnum::InsertStmt(s) => Ok(!s.returning_list.is_empty()),
        NodeEnum::UpdateStmt(s) => Ok(!s.returning_list.is_empty()),
        NodeEnum::DeleteStmt(s) => Ok(!s.returning_list.is_empty()),
        NodeEnum::MergeStmt(s) => Ok(!s.returning_list.is_empty()),
        NodeEnum::SelectStmt(s) => {
            if let Some(with) = s.with_clause.as_ref() {
                for cte_node in with.ctes.iter() {
                    if let Some(NodeEnum::CommonTableExpr(cte)) = cte_node.node.as_ref() {
                        if let Some(q) = cte.ctequery.as_ref() {
                            if let Some(inner) = q.node.as_ref() {
                                if walk(inner)? {
                                    return Ok(true);
                                }
                            }
                        }
                    }
                }
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

#[test]
fn has_returning_equivalence() {
    let corpus = corpus();
    assert!(
        corpus.len() >= 100,
        "corpus must have at least 100 queries (found {})",
        corpus.len()
    );
    let mut mismatches = Vec::new();
    let mut skipped = 0;
    let mut checked = 0;
    for (i, sql) in corpus.iter().enumerate() {
        let oracle = match oracle_has_returning(sql) {
            Ok(v) => v,
            Err(_) => {
                // Unparseable — oracle can't answer. The wrapper's safe
                // default is false; don't count as mismatch. Track for visibility.
                skipped += 1;
                continue;
            }
        };
        let actual = subject_has_returning(sql);
        if actual != oracle {
            mismatches.push(format!(
                "line {}: oracle={} actual={} sql={}",
                i, oracle, actual, sql
            ));
        }
        checked += 1;
    }
    println!(
        "pg_query equivalence: checked {} queries, skipped {} (unparseable), total {}",
        checked,
        skipped,
        corpus.len()
    );
    if skipped > 5 {
        panic!(
            "too many unparseable corpus lines ({}); fix the corpus or adjust the filter",
            skipped
        );
    }
    assert!(
        mismatches.is_empty(),
        "has_returning diverges from pg_query oracle on {} queries:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}
