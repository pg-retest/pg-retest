//! Canonical comparison of result rows. Each row arrives as PostgreSQL's own
//! record-text rendering (e.g. `(1,foo,t)`) so the comparison is type- and
//! column-name-agnostic — PG does the per-type formatting, we just compare strings.

/// True if `sql`'s query carries an ORDER BY (compared in order; otherwise compared as
/// a multiset). Heuristic, case-insensitive — sufficient for the read corpus and, since
/// it is applied identically to candidate and reference, it cannot skew a diff: both
/// sides are sorted, or neither is.
pub fn is_ordered(sql: &str) -> bool {
    sql.to_uppercase().contains("ORDER BY")
}

/// Sentinel distinguishing SQL `NULL` from the empty string `''` in canonical cells.
/// (PostgreSQL record-text renders NULL as a bare-empty field and `''` as `""`; MySQL
/// `--batch -N` renders NULL as the literal `NULL` and `''` as an empty field.)
pub const NULL_SENTINEL: &str = "\u{0}NULL\u{0}";

/// Join one row's canonical cells into a single comparable string (cells separated by a
/// byte that cannot appear in normal SQL text output).
pub fn join_cells(cells: &[String]) -> String {
    cells.join("\u{1}")
}

/// Parse PostgreSQL's record-text rendering of one row — e.g.
/// `(1,alice,,"a,b","x""y","")` — into canonical cells. A bare-empty field is SQL NULL
/// (`NULL_SENTINEL`); `""` is the empty string. Used by the cross-engine live oracle.
pub fn parse_pg_record(record: &str) -> Vec<String> {
    let inner = record
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or(record);
    // A single bare-empty field — PG renders a one-column NULL row as "()".
    if inner.is_empty() {
        return vec![NULL_SENTINEL.to_string()];
    }
    let bytes = inner.as_bytes();
    let mut cells = Vec::new();
    let mut i = 0;
    loop {
        if bytes.get(i) == Some(&b'"') {
            // Quoted field: read to the closing unescaped quote ("" is an escaped ").
            i += 1;
            let mut val: Vec<u8> = Vec::new();
            while i < bytes.len() {
                if bytes[i] == b'"' {
                    if bytes.get(i + 1) == Some(&b'"') {
                        val.push(b'"');
                        i += 2;
                    } else {
                        i += 1; // closing quote
                        break;
                    }
                } else {
                    val.push(bytes[i]);
                    i += 1;
                }
            }
            cells.push(String::from_utf8_lossy(&val).into_owned());
        } else {
            // Unquoted field: read to the next top-level comma or end. Empty => NULL.
            let start = i;
            while i < bytes.len() && bytes[i] != b',' {
                i += 1;
            }
            let raw = &inner[start..i];
            cells.push(if raw.is_empty() {
                NULL_SENTINEL.to_string()
            } else {
                raw.to_string()
            });
        }
        match bytes.get(i) {
            None => break,
            Some(&b',') => {
                i += 1;
                if i == bytes.len() {
                    // Trailing comma → final bare-empty field is NULL.
                    cells.push(NULL_SENTINEL.to_string());
                    break;
                }
            }
            _ => break,
        }
    }
    cells
}

/// Parse one MySQL `--batch -N --raw` output line (tab-separated; literal `NULL` is SQL
/// NULL, an empty field is the empty string `''`) into canonical cells.
pub fn parse_mysql_row(line: &str) -> Vec<String> {
    line.split('\t')
        .map(|c| {
            if c == "NULL" {
                NULL_SENTINEL.to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// True if the two result sets are equivalent under the ordering rule.
pub fn rows_equivalent(candidate: &[String], reference: &[String], ordered: bool) -> bool {
    if candidate.len() != reference.len() {
        return false;
    }
    if ordered {
        candidate == reference
    } else {
        let mut a = candidate.to_vec();
        let mut b = reference.to_vec();
        a.sort();
        b.sort();
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_ordered() {
        assert!(is_ordered("SELECT id FROM t ORDER BY id"));
        assert!(is_ordered("select * from t order by 1"));
        assert!(!is_ordered("SELECT id FROM t"));
    }

    #[test]
    fn test_unordered_is_multiset_equal() {
        let a = vec!["(1)".to_string(), "(2)".to_string()];
        let b = vec!["(2)".to_string(), "(1)".to_string()];
        assert!(rows_equivalent(&a, &b, false));
        assert!(!rows_equivalent(&a, &b, true)); // order matters when ordered
    }

    #[test]
    fn test_ordered_requires_same_order() {
        let a = vec!["(1)".to_string(), "(2)".to_string()];
        assert!(rows_equivalent(&a, &a, true));
    }

    #[test]
    fn test_different_lengths_never_equal() {
        let a = vec!["(1)".to_string()];
        let b = vec!["(1)".to_string(), "(2)".to_string()];
        assert!(!rows_equivalent(&a, &b, false));
    }

    #[test]
    fn test_divergent_string_literal_caught() {
        // The headline, at the row level: regex corrupts the literal value.
        let a = vec!["(\"CASE WHEN x THEN 1 ELSE 0 END\")".to_string()];
        let b = vec!["(\"IF(x,1,0)\")".to_string()];
        assert!(!rows_equivalent(&a, &b, false));
    }

    // --- cross-engine canonical parsers (the false-diff guard) ---

    #[test]
    fn test_parse_pg_record_basic_and_quoting() {
        // 1, alice, NULL(bare), "a,b"(comma-quoted), x"y(quote-escaped), ""(empty string)
        let cells = parse_pg_record(r#"(1,alice,,"a,b","x""y","")"#);
        assert_eq!(
            cells,
            vec![
                "1".to_string(),
                "alice".to_string(),
                NULL_SENTINEL.to_string(),
                "a,b".to_string(),
                "x\"y".to_string(),
                "".to_string(),
            ]
        );
    }

    #[test]
    fn test_parse_pg_record_single_null_and_single_value() {
        assert_eq!(parse_pg_record("()"), vec![NULL_SENTINEL.to_string()]);
        assert_eq!(parse_pg_record("(7)"), vec!["7".to_string()]);
        // A multibyte value survives byte-wise quoted accumulation.
        assert_eq!(parse_pg_record("(café)"), vec!["café".to_string()]);
    }

    #[test]
    fn test_parse_mysql_row_null_vs_empty() {
        assert_eq!(
            parse_mysql_row("1\talice\tNULL\ta,b"),
            vec![
                "1".to_string(),
                "alice".to_string(),
                NULL_SENTINEL.to_string(),
                "a,b".to_string()
            ]
        );
        // empty field = empty string (NOT null)
        assert_eq!(parse_mysql_row(""), vec!["".to_string()]);
    }

    #[test]
    fn test_pg_and_mysql_rows_canonicalize_equal() {
        // Same logical row from each engine must produce the same canonical string.
        let pg = join_cells(&parse_pg_record(r#"(1,alice,,"IF(x,1,0)")"#));
        let mysql = join_cells(&parse_mysql_row("1\talice\tNULL\tIF(x,1,0)"));
        assert_eq!(pg, mysql);
    }

    #[test]
    fn test_null_distinct_from_empty_string_cross_engine() {
        // NULL and '' must NOT canonicalize the same — a real semantic difference.
        let pg_null = join_cells(&parse_pg_record("()"));
        let pg_empty = join_cells(&parse_pg_record(r#"("")"#));
        assert_ne!(pg_null, pg_empty);
        // And each matches its MySQL counterpart.
        assert_eq!(pg_null, join_cells(&parse_mysql_row("NULL")));
        assert_eq!(pg_empty, join_cells(&parse_mysql_row("")));
    }
}
