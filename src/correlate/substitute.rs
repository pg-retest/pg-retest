use std::borrow::Cow;

use dashmap::DashMap;

/// Eligibility state for whether the next literal should be substituted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Eligibility {
    /// Default / neutral: literals are eligible for substitution.
    Neutral,
    /// Next literal is explicitly eligible (after WHERE, AND, =, IN, etc.).
    Eligible,
    /// Next literal should NOT be substituted (after LIMIT, OFFSET, FETCH).
    Ineligible,
    /// Next token should be skipped entirely (after AS, FROM, JOIN, INTO, TABLE, INDEX).
    IdentifierContext,
}

/// Substitute captured IDs in SQL with their replayed counterparts.
///
/// Uses `crate::sql::visit_tokens` (zero-alloc callback API) for hot-path
/// performance. The `Eligibility` state machine tracks SQL keyword context
/// (WHERE/AND/OR → eligible; LIMIT/OFFSET/FETCH → ineligible;
/// AS/FROM/JOIN → identifier context) to decide which literals are eligible
/// for substitution. Returns the (possibly modified) SQL and the count of
/// substitutions made.
pub fn substitute_ids<'a>(sql: &'a str, map: &DashMap<String, String>) -> (Cow<'a, str>, usize) {
    use crate::sql::{visit_tokens, TokenKind};

    if map.is_empty() {
        return (Cow::Borrowed(sql), 0);
    }

    let mut out = String::with_capacity(sql.len());
    let mut count = 0usize;
    let mut eligibility = Eligibility::Neutral;

    visit_tokens(sql, |kind, text| {
        match kind {
            // String literals: substitute inner content if eligible.
            TokenKind::StringLiteral => {
                let inner = strip_single_quotes(text);
                let should_sub =
                    matches!(eligibility, Eligibility::Neutral | Eligibility::Eligible);
                if should_sub {
                    if let Some(replacement) = map.get(inner) {
                        out.push('\'');
                        out.push_str(replacement.value());
                        out.push('\'');
                        count += 1;
                    } else {
                        out.push_str(text);
                    }
                } else {
                    out.push_str(text);
                }
                if eligibility != Eligibility::Neutral {
                    eligibility = Eligibility::Neutral;
                }
            }
            // Numeric literals: substitute whole-token text if eligible AND
            // the replacement is a safe-numeric-shaped string.
            TokenKind::Number => {
                let should_sub =
                    matches!(eligibility, Eligibility::Neutral | Eligibility::Eligible);
                if should_sub {
                    if let Some(replacement) = map.get(text) {
                        let safe = replacement.value().chars().all(|c| {
                            c.is_ascii_digit() || matches!(c, '.' | '-' | 'e' | 'E' | '+')
                        });
                        if safe {
                            out.push_str(replacement.value());
                            count += 1;
                        } else {
                            out.push_str(text);
                        }
                    } else {
                        out.push_str(text);
                    }
                } else {
                    out.push_str(text);
                }
                if eligibility != Eligibility::Neutral {
                    eligibility = Eligibility::Neutral;
                }
            }
            TokenKind::Ident => {
                let upper_buf = text.to_ascii_uppercase();
                match upper_buf.as_str() {
                    "WHERE" | "AND" | "OR" | "ON" | "IN" | "VALUES" | "SET" | "BETWEEN"
                    | "HAVING" => {
                        eligibility = Eligibility::Eligible;
                    }
                    "LIMIT" | "OFFSET" | "FETCH" => {
                        eligibility = Eligibility::Ineligible;
                    }
                    "AS" | "FROM" | "JOIN" | "INTO" | "TABLE" | "INDEX" => {
                        eligibility = Eligibility::IdentifierContext;
                    }
                    _ => {
                        if eligibility == Eligibility::IdentifierContext {
                            eligibility = Eligibility::Neutral;
                        }
                    }
                }
                out.push_str(text);
            }
            TokenKind::QuotedIdent => {
                if eligibility == Eligibility::IdentifierContext {
                    eligibility = Eligibility::Neutral;
                }
                out.push_str(text);
            }
            TokenKind::Punct => {
                // Comparison-opening operators set Eligible. The lexer emits
                // compound operators like `<=`, `>=`, `<>` as sequential
                // single-char Puncts; setting Eligible on the first char is
                // sufficient and matches prior behavior.
                match text {
                    "=" | "<" | ">" => eligibility = Eligibility::Eligible,
                    _ => {}
                }
                out.push_str(text);
            }
            // Whitespace, comments, dollar-strings, bind params pass through.
            _ => out.push_str(text),
        }
    });

    if count == 0 {
        (Cow::Borrowed(sql), 0)
    } else {
        (Cow::Owned(out), count)
    }
}

/// Strip the leading and trailing single quote from a StringLiteral token text.
fn strip_single_quotes(tok_text: &str) -> &str {
    let bytes = tok_text.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'' {
        &tok_text[1..tok_text.len() - 1]
    } else {
        // Unterminated string — strip only the leading quote.
        &tok_text[1..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dashmap::DashMap;

    fn make_map(entries: &[(&str, &str)]) -> DashMap<String, String> {
        let map = DashMap::new();
        for (k, v) in entries {
            map.insert(k.to_string(), v.to_string());
        }
        map
    }

    #[test]
    fn test_integer_in_where() {
        let map = make_map(&[("42", "1001")]);
        let (r, c) = substitute_ids("SELECT * FROM t WHERE id = 42", &map);
        assert_eq!(r, "SELECT * FROM t WHERE id = 1001");
        assert_eq!(c, 1);
    }
    #[test]
    fn test_integer_in_and() {
        let map = make_map(&[("1", "2001"), ("42", "1001")]);
        let (r, c) = substitute_ids("SELECT * FROM t WHERE a = 1 AND b = 42", &map);
        assert_eq!(r, "SELECT * FROM t WHERE a = 2001 AND b = 1001");
        assert_eq!(c, 2);
    }
    #[test]
    fn test_integer_in_values() {
        let map = make_map(&[("42", "1001")]);
        let (r, c) = substitute_ids("INSERT INTO t (id, name) VALUES (42, 'foo')", &map);
        assert_eq!(r, "INSERT INTO t (id, name) VALUES (1001, 'foo')");
        assert_eq!(c, 1);
    }
    #[test]
    fn test_integer_in_set() {
        let map = make_map(&[("42", "1001")]);
        let (r, _c) = substitute_ids("UPDATE t SET order_id = 42 WHERE 1=1", &map);
        assert!(r.contains("order_id = 1001"));
    }
    #[test]
    fn test_integer_in_in_list() {
        let map = make_map(&[("42", "1001"), ("43", "1002"), ("44", "1003")]);
        let (r, c) = substitute_ids("SELECT * FROM t WHERE id IN (42, 43, 44)", &map);
        assert_eq!(r, "SELECT * FROM t WHERE id IN (1001, 1002, 1003)");
        assert_eq!(c, 3);
    }
    #[test]
    fn test_no_substitute_in_limit() {
        let map = make_map(&[("42", "1001")]);
        let (r, _) = substitute_ids("SELECT * FROM t LIMIT 42", &map);
        assert_eq!(r, "SELECT * FROM t LIMIT 42");
    }
    #[test]
    fn test_no_substitute_in_offset() {
        let map = make_map(&[("10", "999")]);
        let (r, _) = substitute_ids("SELECT * FROM t LIMIT 5 OFFSET 10", &map);
        assert_eq!(r, "SELECT * FROM t LIMIT 5 OFFSET 10");
    }
    #[test]
    fn test_no_substitute_partial_match() {
        let map = make_map(&[("42", "1001")]);
        let (r, c) = substitute_ids("SELECT * FROM t WHERE id = 420", &map);
        assert_eq!(r, "SELECT * FROM t WHERE id = 420");
        assert_eq!(c, 0);
    }
    #[test]
    fn test_no_substitute_in_string() {
        let map = make_map(&[("42", "1001")]);
        let (r, c) = substitute_ids("SELECT * FROM t WHERE name = 'item42'", &map);
        assert_eq!(r, "SELECT * FROM t WHERE name = 'item42'");
        assert_eq!(c, 0);
    }
    #[test]
    fn test_no_substitute_in_identifier() {
        let map = make_map(&[("42", "1001")]);
        let (r, c) = substitute_ids("SELECT col42 FROM t", &map);
        assert_eq!(r, "SELECT col42 FROM t");
        assert_eq!(c, 0);
    }
    #[test]
    fn test_uuid_in_where() {
        let src = "550e8400-e29b-41d4-a716-446655440000";
        let dst = "aaaabbbb-cccc-dddd-eeee-ffffffffffff";
        let map = make_map(&[(src, dst)]);
        let sql = format!("SELECT * FROM t WHERE uuid = '{}'", src);
        let (r, c) = substitute_ids(&sql, &map);
        assert_eq!(r, format!("SELECT * FROM t WHERE uuid = '{}'", dst));
        assert_eq!(c, 1);
    }
    #[test]
    fn test_no_map_entries() {
        let map = DashMap::new();
        let (r, c) = substitute_ids("SELECT * FROM t WHERE id = 42", &map);
        assert_eq!(r, "SELECT * FROM t WHERE id = 42");
        assert_eq!(c, 0);
    }
    #[test]
    fn test_multiple_substitutions() {
        let map = make_map(&[("42", "1001"), ("43", "1002"), ("44", "1003")]);
        let (_r, c) = substitute_ids("SELECT * FROM t WHERE id IN (42, 43, 44)", &map);
        assert_eq!(c, 3);
    }
    #[test]
    fn test_dollar_quoted_string() {
        let map = make_map(&[("42", "1001")]);
        let (r, c) = substitute_ids("SELECT $$contains 42$$", &map);
        assert_eq!(r, "SELECT $$contains 42$$");
        assert_eq!(c, 0);
    }
    #[test]
    fn test_escaped_quotes() {
        let map = make_map(&[("42", "1001")]);
        let (r, c) = substitute_ids("WHERE name = 'it''s 42'", &map);
        assert_eq!(r, "WHERE name = 'it''s 42'");
        assert_eq!(c, 0);
    }
    #[test]
    fn test_subquery_with_limit() {
        let map = make_map(&[("42", "1001"), ("5", "999"), ("99", "2002")]);
        let sql =
            "SELECT * FROM t WHERE id = 42 AND status IN (SELECT s FROM t2 LIMIT 5) AND x = 99";
        let (r, c) = substitute_ids(sql, &map);
        assert!(r.contains("id = 1001"));
        assert!(r.contains("LIMIT 5")); // 5 NOT substituted
        assert!(r.contains("x = 2002"));
        assert_eq!(c, 2);
    }
    #[test]
    fn test_eligibility_resets_after_literal() {
        let map = make_map(&[("5", "999"), ("42", "1001")]);
        let sql = "SELECT * FROM t LIMIT 5 WHERE id = 42";
        let (r, _) = substitute_ids(sql, &map);
        assert!(r.contains("LIMIT 5"));
        assert!(r.contains("= 1001"));
    }

    #[test]
    fn test_numeric_substitution_rejects_injection() {
        // A replacement value with SQL injection should be rejected
        let map = make_map(&[("42", "1; DROP TABLE users")]);
        let (r, c) = substitute_ids("SELECT * FROM t WHERE id = 42", &map);
        assert_eq!(r, "SELECT * FROM t WHERE id = 42");
        assert_eq!(c, 0);
    }

    #[test]
    fn test_tagged_dollar_quote() {
        let map = make_map(&[("42", "1001")]);
        let (r, c) = substitute_ids("SELECT $fn$contains 42$fn$", &map);
        assert_eq!(r, "SELECT $fn$contains 42$fn$");
        assert_eq!(c, 0);
    }
}
