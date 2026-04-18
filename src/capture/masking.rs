/// Mask SQL literal values to prevent PII leakage.
///
/// Replaces single-quoted strings with `$S` and numeric literals with `$N`.
/// Handles escaped quotes (`''`), dollar-quoted strings (`$$...$$` and
/// `$tag$...$tag$`), and does not mask digits inside identifiers.
///
/// Uses `crate::sql::visit_tokens` (zero-alloc callback API) for hot-path
/// performance — avoids the per-token `Token` struct allocation the iterator
/// form incurs.
pub fn mask_sql_literals(sql: &str) -> String {
    use crate::sql::{visit_tokens, TokenKind};

    let mut out = String::with_capacity(sql.len());
    visit_tokens(sql, |kind, text| match kind {
        TokenKind::StringLiteral | TokenKind::DollarString => out.push_str("$S"),
        TokenKind::Number => out.push_str("$N"),
        _ => out.push_str(text),
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mask_single_quoted_strings() {
        assert_eq!(
            mask_sql_literals("SELECT * FROM users WHERE name = 'Alice'"),
            "SELECT * FROM users WHERE name = $S"
        );
    }

    #[test]
    fn test_mask_escaped_quotes() {
        assert_eq!(
            mask_sql_literals("INSERT INTO t (s) VALUES ('it''s a test')"),
            "INSERT INTO t (s) VALUES ($S)"
        );
    }

    #[test]
    fn test_mask_numeric_literals() {
        assert_eq!(
            mask_sql_literals("SELECT * FROM users WHERE id = 42"),
            "SELECT * FROM users WHERE id = $N"
        );
    }

    #[test]
    fn test_mask_decimal_numbers() {
        assert_eq!(
            mask_sql_literals("SELECT * FROM t WHERE price > 19.99"),
            "SELECT * FROM t WHERE price > $N"
        );
    }

    #[test]
    fn test_preserve_identifiers_with_numbers() {
        assert_eq!(
            mask_sql_literals("SELECT col1, col2 FROM table3"),
            "SELECT col1, col2 FROM table3"
        );
    }

    #[test]
    fn test_mask_dollar_quoted_strings() {
        assert_eq!(mask_sql_literals("SELECT $$hello world$$"), "SELECT $S");
    }

    #[test]
    fn test_mask_multiple_values() {
        assert_eq!(
            mask_sql_literals("INSERT INTO t (a, b) VALUES ('hello', 42)"),
            "INSERT INTO t (a, b) VALUES ($S, $N)"
        );
    }

    #[test]
    fn test_preserve_double_quoted_identifiers() {
        assert_eq!(
            mask_sql_literals("SELECT \"column1\" FROM \"table2\" WHERE id = 5"),
            "SELECT \"column1\" FROM \"table2\" WHERE id = $N"
        );
    }

    #[test]
    fn test_no_literals() {
        assert_eq!(
            mask_sql_literals("SELECT count(*) FROM users"),
            "SELECT count(*) FROM users"
        );
    }

    #[test]
    fn test_negative_number() {
        assert_eq!(
            mask_sql_literals("SELECT * FROM t WHERE x = -5"),
            "SELECT * FROM t WHERE x = $N"
        );
    }
}
