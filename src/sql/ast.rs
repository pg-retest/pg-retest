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
pub fn inject_returning(sql: &str, pk_map: &[TablePk]) -> Result<Option<String>, AstError> {
    if !might_have_returning(sql) {
        return Ok(None);
    }
    // Skip if it already has RETURNING.
    if has_returning(sql)? {
        return Ok(None);
    }
    let parsed = pg_query::parse(sql).map_err(|e| AstError::Parse(format!("{}", e)))?;
    // Locate the top-level INSERT. Phase 2 only handles non-CTE-wrapped.
    let insert = find_top_level_insert(&parsed);
    let Some(insert) = insert else {
        return Ok(None);
    };
    // Table lookup.
    let Some((schema, table)) = extract_insert_target(insert) else {
        return Ok(None);
    };
    let pk = pk_map
        .iter()
        .find(|pk| pk.table == table && (schema.is_empty() || pk.schema == schema));
    let Some(pk) = pk else {
        return Ok(None);
    };
    let returning_cols = pk.columns.join(", ");
    let splice_offset = find_splice_offset(sql, insert);
    let before = sql[..splice_offset].trim_end();
    let after = &sql[splice_offset..];
    // Ensure a separator between `RETURNING cols` and `after` when `after`
    // starts with a structural keyword (e.g. `ON CONFLICT`) that we spliced
    // before. The trim_end() above already chewed the space preceding the
    // splice point. Punctuation like `;` needs no separator.
    let sep = match after.chars().next() {
        Some(c) if c.is_ascii_alphabetic() => " ",
        _ => "",
    };
    Ok(Some(format!(
        "{} RETURNING {}{}{}",
        before, returning_cols, sep, after
    )))
}

/// Locate the first top-level InsertStmt in the parsed tree. Returns None
/// for CTE-wrapped INSERTs (top-level is SelectStmt) — Phase 2 scope skips
/// those since the splice semantics are ambiguous.
fn find_top_level_insert(
    parsed: &pg_query::ParseResult,
) -> Option<&pg_query::protobuf::InsertStmt> {
    use pg_query::NodeEnum;
    for stmt in parsed.protobuf.stmts.iter() {
        if let Some(NodeEnum::InsertStmt(insert)) = stmt.stmt.as_ref().and_then(|s| s.node.as_ref())
        {
            return Some(insert.as_ref());
        }
    }
    None
}

/// Extract (schema, table) from an InsertStmt's relation field.
/// Schema is empty when the insert target is unqualified.
fn extract_insert_target(stmt: &pg_query::protobuf::InsertStmt) -> Option<(String, String)> {
    let rel = stmt.relation.as_ref()?;
    Some((rel.schemaname.clone(), rel.relname.clone()))
}

/// Compute the byte offset at which RETURNING should be spliced into `sql`.
/// RETURNING goes at the END of the statement in all cases, per PG grammar:
///
///     INSERT ... VALUES (...) [ ON CONFLICT ... ] [ RETURNING ... ]
///
/// Walks backward from end of `sql`, skipping trailing whitespace, -- line
/// comments, /* block */ comments, and a single trailing semicolon.
fn find_splice_offset(sql: &str, _stmt: &pg_query::protobuf::InsertStmt) -> usize {
    end_of_statement_offset(sql)
}

/// Walk backward from end of `sql`, skipping trailing whitespace, line
/// comments, block comments, and a single trailing semicolon. Returns the
/// byte offset where RETURNING should be spliced.
fn end_of_statement_offset(sql: &str) -> usize {
    let bytes = sql.as_bytes();
    let mut end = bytes.len();

    loop {
        let before = end;
        // Strip trailing whitespace.
        while end > 0 && bytes[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        // Strip trailing line comment. `--` starts a line comment that runs
        // to the next `\n` or EOF. Here we only care about a `--` on the
        // last line (anything after a prior `\n`). Scan the last line from
        // its start, respecting single-quoted string literals with `''`
        // escape so we don't mistake `'--'` for a comment start.
        let line_start = sql[..end].rfind('\n').map(|nl| nl + 1).unwrap_or(0);
        if let Some(dd) = find_line_comment_start(&sql[line_start..end]) {
            end = line_start + dd;
            continue;
        }
        // Strip trailing block comment.
        if end >= 2 && &sql[end - 2..end] == "*/" {
            if let Some(open) = sql[..end - 2].rfind("/*") {
                end = open;
                continue;
            }
        }
        // Strip a single trailing semicolon.
        if end > 0 && bytes[end - 1] == b';' {
            end -= 1;
            continue;
        }
        if end == before {
            break;
        }
    }
    end
}

/// Find the byte offset of `--` in `line` that starts a SQL line comment,
/// ignoring occurrences inside single-quoted string literals. Returns
/// `None` if there is no unquoted `--`.
fn find_line_comment_start(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_string {
            if b == b'\'' {
                // Escaped '' inside a string literal.
                if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                    i += 2;
                    continue;
                }
                in_string = false;
            }
        } else if b == b'\'' {
            in_string = true;
        } else if b == b'-' && i + 1 < bytes.len() && bytes[i + 1] == b'-' {
            return Some(i);
        }
        i += 1;
    }
    None
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

    fn pk_orders() -> Vec<TablePk> {
        vec![TablePk {
            schema: "public".into(),
            table: "orders".into(),
            columns: vec!["id".into()],
        }]
    }

    #[test]
    fn ast_inject_simple() {
        let pk = pk_orders();
        assert_eq!(
            inject_returning("INSERT INTO orders (name) VALUES ('test')", &pk).unwrap(),
            Some("INSERT INTO orders (name) VALUES ('test') RETURNING id".into())
        );
    }

    #[test]
    fn ast_inject_already_has_returning() {
        let pk = pk_orders();
        assert_eq!(
            inject_returning(
                "INSERT INTO orders (name) VALUES ('test') RETURNING id",
                &pk
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn ast_inject_unknown_table() {
        let pk = pk_orders();
        assert_eq!(
            inject_returning("INSERT INTO unknown (name) VALUES ('test')", &pk).unwrap(),
            None
        );
    }

    #[test]
    fn ast_inject_on_conflict_splice_before() {
        // Name retained for historical reasons; RETURNING now correctly appears
        // AFTER ON CONFLICT per PG grammar (INSERT ... ON CONFLICT ... RETURNING ...).
        let pk = pk_orders();
        assert_eq!(
            inject_returning(
                "INSERT INTO orders (id, name) VALUES (1, 'test') ON CONFLICT DO NOTHING",
                &pk
            )
            .unwrap(),
            Some(
                "INSERT INTO orders (id, name) VALUES (1, 'test') ON CONFLICT DO NOTHING RETURNING id"
                    .into()
            )
        );
    }

    #[test]
    fn ast_inject_on_conflict_do_update() {
        let pk = pk_orders();
        assert_eq!(
            inject_returning(
                "INSERT INTO orders (id, name) VALUES (1, 'test') ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
                &pk
            )
            .unwrap(),
            Some(
                "INSERT INTO orders (id, name) VALUES (1, 'test') ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name RETURNING id"
                    .into()
            )
        );
    }

    #[test]
    fn ast_inject_multi_row_values() {
        let pk = pk_orders();
        assert_eq!(
            inject_returning("INSERT INTO orders (name) VALUES ('a'), ('b'), ('c')", &pk).unwrap(),
            Some("INSERT INTO orders (name) VALUES ('a'), ('b'), ('c') RETURNING id".into())
        );
    }

    #[test]
    fn ast_inject_insert_select() {
        let pk = pk_orders();
        assert_eq!(
            inject_returning(
                "INSERT INTO orders (id, name) SELECT id, name FROM staging",
                &pk
            )
            .unwrap(),
            Some("INSERT INTO orders (id, name) SELECT id, name FROM staging RETURNING id".into())
        );
    }

    #[test]
    fn ast_inject_trailing_semicolon() {
        let pk = pk_orders();
        assert_eq!(
            inject_returning("INSERT INTO orders (name) VALUES ('x');", &pk).unwrap(),
            Some("INSERT INTO orders (name) VALUES ('x') RETURNING id;".into())
        );
    }

    #[test]
    fn ast_inject_trailing_comment() {
        let pk = pk_orders();
        assert_eq!(
            inject_returning("INSERT INTO orders (name) VALUES ('x') -- trailing", &pk).unwrap(),
            Some("INSERT INTO orders (name) VALUES ('x') RETURNING id -- trailing".into())
        );
    }

    #[test]
    fn ast_inject_cte_wrapped_returns_none() {
        // Phase 2 scope: CTE-wrapped inserts return None (splice semantics
        // ambiguous). Phase 3 may revisit.
        let pk = pk_orders();
        let sql =
            "WITH new_order AS (INSERT INTO orders (name) VALUES ('x')) SELECT * FROM new_order";
        assert_eq!(inject_returning(sql, &pk).unwrap(), None);
    }

    #[test]
    fn ast_inject_schema_qualified() {
        let pk = vec![TablePk {
            schema: "analytics".into(),
            table: "events".into(),
            columns: vec!["event_id".into()],
        }];
        assert_eq!(
            inject_returning("INSERT INTO analytics.events (name) VALUES ('x')", &pk).unwrap(),
            Some("INSERT INTO analytics.events (name) VALUES ('x') RETURNING event_id".into())
        );
    }

    #[test]
    fn ast_inject_not_insert() {
        let pk = pk_orders();
        // UPDATE / DELETE aren't injected even if they target known-PK tables.
        assert_eq!(
            inject_returning("UPDATE orders SET name = 'x' WHERE id = 1", &pk).unwrap(),
            None
        );
        assert_eq!(
            inject_returning("DELETE FROM orders WHERE id = 1", &pk).unwrap(),
            None
        );
    }
}
