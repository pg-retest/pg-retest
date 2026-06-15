//! `PolyglotTransformer` — AST-grade SQL dialect transpilation behind the existing
//! `SqlTransformer` trait, using the MIT `polyglot-sql` crate. **Experimental**:
//! only compiled with the `polyglot-transform` feature (OFF by default).
//!
//! Spec: `retest-research/plan/include/polyglot-sql-transform.md` (FR-XFORM-5/6/9).
//!
//! Honesty posture ("never silently mistranslate"), in layers:
//!  1. `TranspileOptions::strict()` (`UnsupportedLevel::Raise`) instead of the crate
//!     default `Warn` (which emits lossy output). Catches the diagnostics polyglot
//!     itself flags, plus parse/tokenize/syntax errors → `Skipped`.
//!  2. Multi-statement transpile output → `Skipped` (never joined — joining corrupts
//!     statement boundaries, timing, row-count attribution, and txn correlation).
//!  3. **pg_query validity gate**: re-parse the transpiled output with PostgreSQL's
//!     own parser (libpg_query, already a pg-retest dependency). If PG rejects it,
//!     `Skipped`. This is required because — verified empirically against polyglot
//!     0.5.4 — `strict()` does NOT raise on many MySQL-specific constructs that are
//!     invalid in PostgreSQL (`ON DUPLICATE KEY UPDATE`, `REPLACE INTO`, `USE INDEX`,
//!     `STRAIGHT_JOIN`, multi-table `DELETE`, …); it passes them through unchanged.
//!     Without this gate the transformer would emit invalid PG as if it were a
//!     faithful translation. The gate proves *syntactic* PG validity only — never
//!     behavioral equivalence (autocommit, LAST_INSERT_ID, zero-dates, …): see spec §1.

use polyglot_sql::{Dialect, DialectType, Error as PgError, TranspileOptions};

use super::{SqlTransformer, TransformPipeline, TransformResult};

/// Transpiles one SQL statement from `source` dialect to `target` (always
/// PostgreSQL for pg-retest), flag-and-skipping anything it cannot faithfully and
/// validly translate.
pub struct PolyglotTransformer {
    source: DialectType,
    target: DialectType,
    /// Cached source-dialect handle. Built once (not per `transform()` call) because
    /// transform runs offline over potentially millions of captured statements with
    /// no DB round-trip to amortize a per-query rebuild against. Safe to store by
    /// value: `Dialect` is documented `Send + Sync` (the `SqlTransformer` bound).
    dialect: Dialect,
}

impl PolyglotTransformer {
    pub fn new(source: DialectType, target: DialectType) -> Self {
        Self {
            source,
            target,
            dialect: Dialect::get(source),
        }
    }
}

impl SqlTransformer for PolyglotTransformer {
    fn name(&self) -> &str {
        "polyglot"
    }

    fn transform(&self, sql: &str) -> TransformResult {
        if self.source == self.target {
            return TransformResult::Unchanged; // identity — no work
        }

        // strict() = UnsupportedLevel::Raise (NOT the crate default Warn, which is
        // silently lossy). Mandatory: never construct with default options.
        match self
            .dialect
            .transpile_with(sql, self.target, TranspileOptions::strict())
        {
            // Exactly one statement: gate it through PostgreSQL's own parser.
            Ok(stmts) if stmts.len() == 1 => {
                let out = stmts.into_iter().next().unwrap();
                if pg_query::parse(&out).is_ok() {
                    TransformResult::Transformed(out)
                } else {
                    // polyglot emitted PG-invalid output without raising (see module
                    // docs). Flag-and-skip rather than replay a wrong rewrite.
                    TransformResult::Skipped {
                        reason: format!(
                            "polyglot: transpiled output is not valid PostgreSQL: {}",
                            preview(&out)
                        ),
                    }
                }
            }
            // 0 or >1 statements: never join — that corrupts statement boundaries,
            // timing, row-count attribution, and txn/correlation at replay.
            Ok(stmts) => TransformResult::Skipped {
                reason: format!(
                    "polyglot: source produced {} statements (expected exactly 1)",
                    stmts.len()
                ),
            },
            Err(PgError::Unsupported { feature, dialect }) => TransformResult::Skipped {
                reason: format!("polyglot: `{feature}` unsupported in {dialect}"),
            },
            Err(e @ (PgError::Parse { .. } | PgError::Syntax { .. } | PgError::Tokenize { .. })) => {
                TransformResult::Skipped {
                    reason: format!("polyglot: unparseable source SQL: {e}"),
                }
            }
            Err(e) => TransformResult::Skipped {
                reason: format!("polyglot: {e}"),
            },
        }
    }
}

/// First ~80 chars of a string, for skip-reason previews.
fn preview(s: &str) -> String {
    s.chars().take(80).collect()
}

/// MySQL→PostgreSQL transpile pipeline (the AST-grade counterpart of the legacy
/// `mysql_to_pg_pipeline`). FR-XFORM-9: a capture path selects ONE engine; this does
/// not chain regex after polyglot.
pub fn mysql_to_pg_polyglot_pipeline() -> TransformPipeline {
    TransformPipeline::new(vec![Box::new(PolyglotTransformer::new(
        DialectType::MySQL,
        DialectType::PostgreSQL,
    ))])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mysql() -> PolyglotTransformer {
        PolyglotTransformer::new(DialectType::MySQL, DialectType::PostgreSQL)
    }

    fn transformed(r: TransformResult) -> String {
        match r {
            TransformResult::Transformed(s) => s,
            other => panic!("expected Transformed, got {other:?}"),
        }
    }

    #[test]
    fn test_name_is_polyglot() {
        assert_eq!(mysql().name(), "polyglot");
    }

    // --- parity with the legacy regex rules (FR-XFORM-14) ---

    #[test]
    fn test_ifnull_to_coalesce() {
        let out = transformed(mysql().transform("SELECT IFNULL(name, 'x') FROM t"));
        assert!(out.contains("COALESCE"), "got: {out}");
        assert!(!out.to_uppercase().contains("IFNULL"), "got: {out}");
    }

    #[test]
    fn test_backtick_identifiers_become_double_quoted() {
        let out = transformed(mysql().transform("SELECT `id`, `name` FROM `users`"));
        assert!(!out.contains('`'), "backticks should be gone: {out}");
        assert!(out.contains('"'), "expected double-quoted idents: {out}");
    }

    #[test]
    fn test_limit_offset_rewrite() {
        let out = transformed(mysql().transform("SELECT * FROM t LIMIT 10, 20"));
        assert!(out.contains("OFFSET 10"), "got: {out}");
        assert!(out.contains("LIMIT 20"), "got: {out}");
    }

    #[test]
    fn test_unix_timestamp_rewrite_is_valid_pg() {
        // Real polyglot output: EXTRACT(epoch FROM CURRENT_TIMESTAMP) — valid PG.
        let out = transformed(mysql().transform("SELECT UNIX_TIMESTAMP()"));
        assert!(out.to_uppercase().contains("EXTRACT"), "got: {out}");
        assert!(pg_query::parse(&out).is_ok(), "output must be valid PG: {out}");
    }

    // --- THE HEADLINE: AST awareness regex cannot match (FR-XFORM-14) ---

    #[test]
    fn test_if_inside_string_literal_is_preserved() {
        // Regex `IfToCase` rewrites the text INSIDE this string literal, silently
        // changing the string's VALUE. The AST transpiler leaves the literal intact.
        let out = transformed(mysql().transform("SELECT 'IF(x,1,0)'"));
        assert!(
            out.contains("'IF(x,1,0)'"),
            "string literal must be preserved verbatim, got: {out}"
        );
        assert!(
            !out.to_uppercase().contains("CASE"),
            "must NOT inject CASE into a string literal, got: {out}"
        );
    }

    // --- honesty: flag-and-skip, never mistranslate (FR-XFORM-6) ---

    #[test]
    fn test_identity_source_equals_target_is_unchanged() {
        let pg = PolyglotTransformer::new(DialectType::PostgreSQL, DialectType::PostgreSQL);
        assert_eq!(pg.transform("SELECT 1"), TransformResult::Unchanged);
    }

    #[test]
    fn test_unparseable_source_is_skipped() {
        assert!(matches!(
            mysql().transform("this is not valid sql !!!"),
            TransformResult::Skipped { .. }
        ));
    }

    #[test]
    fn test_multi_statement_output_is_skipped_not_joined() {
        assert!(matches!(
            mysql().transform("SELECT 1; SELECT 2"),
            TransformResult::Skipped { .. }
        ));
    }

    #[test]
    fn test_pg_invalid_passthrough_is_skipped_by_validity_gate() {
        // polyglot 0.5.4 passes this through UNCHANGED (it does not raise), but
        // PostgreSQL's parser rejects ON DUPLICATE KEY UPDATE. The pg_query validity
        // gate must catch it and Skip rather than emit invalid PG as "translated".
        let sql = "INSERT INTO t (id) VALUES (1) ON DUPLICATE KEY UPDATE id = id";
        assert!(matches!(
            mysql().transform(sql),
            TransformResult::Skipped { .. }
        ));
    }

    #[test]
    fn test_every_transformed_output_reparses_as_valid_pg() {
        // The honesty guarantee in one assertion: whatever the transformer reports as
        // Transformed must be syntactically valid PostgreSQL.
        let corpus = [
            "SELECT IFNULL(name, 'x') FROM t",
            "SELECT `id`, `name` FROM `users`",
            "SELECT * FROM t LIMIT 10, 20",
            "SELECT CONCAT('a', 'b')",
            "SELECT UNIX_TIMESTAMP()",
            "SELECT 'IF(x,1,0)'",
        ];
        for sql in corpus {
            if let TransformResult::Transformed(out) = mysql().transform(sql) {
                assert!(
                    pg_query::parse(&out).is_ok(),
                    "Transformed output is not valid PG: {sql} -> {out}"
                );
            }
        }
    }

    // --- the labelled fallback factory (FR-XFORM-9) ---

    #[test]
    fn test_mysql_to_pg_polyglot_pipeline_transforms() {
        let pipeline = mysql_to_pg_polyglot_pipeline();
        let out = transformed(pipeline.apply("SELECT IFNULL(a, b) FROM t"));
        assert!(out.contains("COALESCE"), "got: {out}");
    }
}
