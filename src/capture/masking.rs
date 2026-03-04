/// Mask SQL literal values to prevent PII leakage.
///
/// Replaces single-quoted strings with `$S` and numeric literals with `$N`.
/// Handles escaped quotes (`''`), dollar-quoted strings (`$$...$$`),
/// and does not mask numbers in identifiers.
pub fn mask_sql_literals(sql: &str) -> String {
    let chars: Vec<char> = sql.chars().collect();
    let len = chars.len();
    let mut result = String::with_capacity(len);
    let mut i = 0;

    while i < len {
        match chars[i] {
            // Single-quoted string literal
            '\'' => {
                result.push_str("$S");
                i += 1;
                // Skip contents until closing quote
                while i < len {
                    if chars[i] == '\'' {
                        if i + 1 < len && chars[i + 1] == '\'' {
                            // Escaped quote (''), skip both
                            i += 2;
                        } else {
                            // End of string literal
                            i += 1;
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            // Dollar-quoted string ($$...$$)
            '$' if i + 1 < len && chars[i + 1] == '$' => {
                result.push_str("$S");
                i += 2;
                // Skip until matching $$
                while i + 1 < len {
                    if chars[i] == '$' && chars[i + 1] == '$' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                // Handle edge case: reached end without closing $$
                if i >= len {
                    // Already consumed everything
                }
            }
            // Numeric literal (not inside an identifier)
            c if c.is_ascii_digit()
                || (c == '-' && is_numeric_context(&result, &chars, i, len)) =>
            {
                // Check this isn't part of an identifier (letter/underscore before it)
                if c.is_ascii_digit() && is_part_of_identifier(&result) {
                    result.push(c);
                    i += 1;
                } else {
                    result.push_str("$N");
                    // Skip the negative sign if present
                    if c == '-' {
                        i += 1;
                    }
                    // Skip digits
                    while i < len && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                    // Skip decimal part
                    if i < len && chars[i] == '.' && i + 1 < len && chars[i + 1].is_ascii_digit() {
                        i += 1; // skip dot
                        while i < len && chars[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                    // Skip scientific notation
                    if i < len && (chars[i] == 'e' || chars[i] == 'E') {
                        i += 1;
                        if i < len && (chars[i] == '+' || chars[i] == '-') {
                            i += 1;
                        }
                        while i < len && chars[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                }
            }
            // Double-quoted identifier — pass through unchanged
            '"' => {
                result.push(chars[i]);
                i += 1;
                while i < len {
                    result.push(chars[i]);
                    if chars[i] == '"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            // Everything else: pass through
            c => {
                result.push(c);
                i += 1;
            }
        }
    }

    result
}

/// Check if the last character in result is part of an identifier (letter, digit, underscore).
fn is_part_of_identifier(result: &str) -> bool {
    result
        .chars()
        .last()
        .map(|c| c.is_ascii_alphanumeric() || c == '_')
        .unwrap_or(false)
}

/// Check if a '-' at position i looks like a negative number sign rather than subtraction.
fn is_numeric_context(result: &str, chars: &[char], i: usize, len: usize) -> bool {
    // '-' is a negative sign if:
    // 1. The next char is a digit
    // 2. The previous non-whitespace token is an operator or keyword context
    if i + 1 >= len || !chars[i + 1].is_ascii_digit() {
        return false;
    }
    // Look at the last non-whitespace char in result
    let last = result.trim_end().chars().last();
    matches!(
        last,
        Some('(' | ',' | '=' | '<' | '>' | '+' | '-' | '*' | '/' | '|') | None
    )
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
