//! AST-backed SQL analysis for structural questions (RETURNING detection,
//! RETURNING splice points). Uses `pg_query` (Rust bindings to libpg_query —
//! Postgres's own C parser) for grammar fidelity: anything PG accepts,
//! pg_query parses by construction.
//!
//! Hot-path consumers should apply a cheap prefix pre-filter (see
//! `might_have_returning`) before calling the full-parse functions — pg_query
//! costs ~2-20µs per query depending on complexity.

use crate::correlate::capture::TablePk;

/// Errors returned by the AST-backed functions in this module.
#[derive(Debug, thiserror::Error)]
pub enum AstError {
    #[error("pg_query parse failed: {0}")]
    Parse(String),
    #[error("unexpected AST shape: {0}")]
    Shape(String),
}

/// Cheap prefix check: if this returns false, the SQL definitely has no
/// RETURNING clause we care about and the full parse can be skipped.
/// If it returns true, a full parse is required to answer correctly.
///
/// Matches any SQL whose first keyword (ignoring leading whitespace and
/// `--` / `/* */` comments) is INSERT, UPDATE, DELETE, MERGE, or WITH
/// (the last catches CTE-wrapped writes).
pub fn might_have_returning(sql: &str) -> bool {
    let trimmed = skip_leading_whitespace_and_comments(sql);
    let upper_prefix: String = trimmed
        .chars()
        .take(8)
        .collect::<String>()
        .to_ascii_uppercase();
    upper_prefix.starts_with("INSERT")
        || upper_prefix.starts_with("UPDATE")
        || upper_prefix.starts_with("DELETE")
        || upper_prefix.starts_with("MERGE")
        || upper_prefix.starts_with("WITH")
}

/// Skip leading whitespace and SQL comments (`--` line, `/* */` block).
/// Returns a slice of `sql` starting at the first non-skipped byte.
fn skip_leading_whitespace_and_comments(sql: &str) -> &str {
    let mut rest = sql;
    loop {
        let trimmed = rest.trim_start();
        if let Some(stripped) = trimmed.strip_prefix("--") {
            rest = match stripped.find('\n') {
                Some(idx) => &stripped[idx + 1..],
                None => "",
            };
        } else if let Some(stripped) = trimmed.strip_prefix("/*") {
            rest = match stripped.find("*/") {
                Some(idx) => &stripped[idx + 2..],
                None => "",
            };
        } else {
            return trimmed;
        }
    }
}

/// AST-backed `has_returning`. Returns `Ok(true)` iff the top-level statement
/// (or any CTE nested under a top-level `WITH`) is an INSERT, UPDATE, DELETE,
/// or MERGE with a non-empty `returning_list`. Returns `Ok(false)` for SELECT,
/// DDL, and DML without RETURNING. Returns `Err(AstError::Parse)` on
/// syntactically invalid input — callers should treat this as the safe
/// default (usually "assume no RETURNING").
pub fn has_returning(sql: &str) -> Result<bool, AstError> {
    if !might_have_returning(sql) {
        return Ok(false);
    }
    let parsed = pg_query::parse(sql).map_err(|e| AstError::Parse(format!("{}", e)))?;
    for raw in parsed.protobuf.stmts.iter() {
        if let Some(node) = raw.stmt.as_ref().and_then(|s| s.node.as_ref()) {
            if node_has_returning(node)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Walk a single statement node and detect RETURNING. Recurses into
/// `WITH`-wrapped writes: a SELECT with a `with_clause` whose CTE queries
/// include a DML with RETURNING counts as having RETURNING.
fn node_has_returning(node: &pg_query::NodeEnum) -> Result<bool, AstError> {
    use pg_query::NodeEnum as N;
    match node {
        N::InsertStmt(s) => Ok(!s.returning_list.is_empty()),
        N::UpdateStmt(s) => Ok(!s.returning_list.is_empty()),
        N::DeleteStmt(s) => Ok(!s.returning_list.is_empty()),
        N::MergeStmt(s) => Ok(!s.returning_list.is_empty()),
        // Top-level SELECT may wrap a data-modifying CTE.
        N::SelectStmt(s) => {
            if let Some(with) = s.with_clause.as_ref() {
                for cte_node in with.ctes.iter() {
                    if let Some(N::CommonTableExpr(cte)) = cte_node.node.as_ref() {
                        if let Some(q) = cte.ctequery.as_ref() {
                            if let Some(inner) = q.node.as_ref() {
                                if node_has_returning(inner)? {
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

/// AST-backed `inject_returning`. Uses pg_query to identify the splice point
/// and falls back to string splicing (not deparse) so comments and whitespace
/// are preserved. Returns `Ok(None)` if the SQL isn't a bare INSERT targeting
/// a known-PK table, or already has RETURNING.
///
/// Stub until Task 7.
pub fn inject_returning(sql: &str, _pk_map: &[TablePk]) -> Result<Option<String>, AstError> {
    if !might_have_returning(sql) {
        return Ok(None);
    }
    Err(AstError::Shape(
        "inject_returning: pg_query splice not implemented yet (Task 7)".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefilter_skips_select() {
        assert!(!might_have_returning("SELECT * FROM users"));
    }

    #[test]
    fn prefilter_catches_insert() {
        assert!(might_have_returning("INSERT INTO t VALUES (1)"));
        assert!(might_have_returning("  INSERT INTO t VALUES (1)"));
        assert!(might_have_returning("insert into t values (1)"));
    }

    #[test]
    fn prefilter_catches_with_cte() {
        assert!(might_have_returning("WITH x AS (SELECT 1) SELECT * FROM x"));
    }

    #[test]
    fn prefilter_catches_update_delete_merge() {
        assert!(might_have_returning("UPDATE t SET a = 1"));
        assert!(might_have_returning("DELETE FROM t"));
        assert!(might_have_returning("MERGE INTO t USING s ON t.id = s.id"));
    }

    #[test]
    fn prefilter_skips_leading_line_comment() {
        assert!(!might_have_returning("-- comment\nSELECT 1"));
    }

    #[test]
    fn prefilter_skips_leading_block_comment() {
        assert!(!might_have_returning("/* block */ SELECT 1"));
    }

    #[test]
    fn prefilter_catches_insert_after_comment() {
        assert!(might_have_returning(
            "-- inserting\nINSERT INTO t VALUES (1)"
        ));
    }

    #[test]
    fn ast_has_returning_simple() {
        assert!(has_returning("INSERT INTO t VALUES (1) RETURNING id").unwrap());
        assert!(!has_returning("INSERT INTO t VALUES (1)").unwrap());
    }

    #[test]
    fn ast_has_returning_update_delete() {
        assert!(has_returning("UPDATE t SET a = 1 RETURNING id").unwrap());
        assert!(has_returning("DELETE FROM t WHERE id = 1 RETURNING id").unwrap());
        assert!(!has_returning("UPDATE t SET a = 1").unwrap());
    }

    #[test]
    fn ast_has_returning_cte_wrapped() {
        let sql = "WITH new_order AS (INSERT INTO orders (customer_id) VALUES (42) RETURNING id) SELECT * FROM new_order";
        assert!(has_returning(sql).unwrap());
    }

    #[test]
    fn ast_has_returning_cte_no_returning_inside() {
        let sql = "WITH x AS (SELECT 1) INSERT INTO t SELECT * FROM x";
        assert!(!has_returning(sql).unwrap());
    }

    #[test]
    fn ast_has_returning_returning_as_column_alias() {
        // A column named "returning" must NOT trigger true — bug class in legacy.
        let sql = "SELECT col AS \"returning\" FROM t";
        assert!(!has_returning(sql).unwrap());
    }

    #[test]
    fn ast_has_returning_returning_in_comment() {
        assert!(!has_returning("-- RETURNING\nSELECT 1").unwrap());
        assert!(!has_returning("/* RETURNING id */ SELECT 1").unwrap());
    }

    #[test]
    fn ast_has_returning_invalid_sql() {
        // Unparseable returns Err — callers map to safe default.
        let result = has_returning("INSERT INTO");
        assert!(result.is_err(), "expected parse error for truncated INSERT");
    }

    #[test]
    fn ast_has_returning_string_contains_returning() {
        let sql = "INSERT INTO t (s) VALUES ('it has RETURNING in it')";
        assert!(!has_returning(sql).unwrap());
    }
}
