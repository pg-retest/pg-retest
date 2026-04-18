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
/// (after stripping any enclosing `WITH` CTE wrapper) is an INSERT, UPDATE,
/// DELETE, or MERGE with a non-empty `returningList`. Returns `Ok(false)`
/// for SELECT, DDL, and DML without RETURNING. Returns `Err(AstError::Parse)`
/// on syntactically invalid input — callers should treat this as the safe
/// default (usually "assume no RETURNING").
///
/// Stub until Task 5: always returns Err(Shape) when the prefilter passes,
/// so tests written against the final shape fail loudly.
pub fn has_returning(sql: &str) -> Result<bool, AstError> {
    if !might_have_returning(sql) {
        return Ok(false);
    }
    Err(AstError::Shape(
        "has_returning: pg_query traversal not implemented yet (Task 5)".into(),
    ))
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
    fn stub_has_returning_returns_ok_false_for_non_candidate() {
        // The stub must correctly short-circuit on the prefilter.
        assert!(!has_returning("SELECT 1").unwrap());
    }

    #[test]
    fn stub_has_returning_returns_err_for_candidate() {
        // The stub must return Err (not panic, not wrong answer) for candidates.
        // Tasks 5 replaces this behavior with a real walk.
        assert!(has_returning("INSERT INTO t VALUES (1)").is_err());
    }
}
