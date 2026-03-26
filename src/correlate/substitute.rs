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

/// Parser state for SQL character-level scanning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Normal,
    InStringLiteral,
    InIdentifier,
    InLineComment,
    InBlockComment,
    InNumericLiteral,
}

/// Substitute captured IDs in SQL with their replayed counterparts.
///
/// Walks the SQL character by character, tracking parser state and keyword context
/// to determine which literals are eligible for substitution. Returns the
/// (possibly modified) SQL and the count of substitutions made.
pub fn substitute_ids<'a>(sql: &'a str, map: &DashMap<String, String>) -> (Cow<'a, str>, usize) {
    if map.is_empty() {
        return (Cow::Borrowed(sql), 0);
    }

    let chars: Vec<char> = sql.chars().collect();
    let len = chars.len();
    let mut result = String::with_capacity(len);
    let mut count = 0usize;
    let mut i = 0;
    let mut state = State::Normal;
    let mut eligibility = Eligibility::Neutral;
    let mut string_buf = String::new();

    while i < len {
        match state {
            State::Normal => {
                let ch = chars[i];

                // Line comment: --
                if ch == '-' && i + 1 < len && chars[i + 1] == '-' {
                    result.push('-');
                    result.push('-');
                    i += 2;
                    state = State::InLineComment;
                    continue;
                }

                // Block comment: /*
                if ch == '/' && i + 1 < len && chars[i + 1] == '*' {
                    result.push('/');
                    result.push('*');
                    i += 2;
                    state = State::InBlockComment;
                    continue;
                }

                // Dollar-quoted string: $$ or $tag$
                if ch == '$' {
                    // Collect tag chars after the opening $
                    let tag_start = i;
                    let mut tag_end = i + 1;
                    while tag_end < len
                        && (chars[tag_end].is_ascii_alphanumeric() || chars[tag_end] == '_')
                    {
                        tag_end += 1;
                    }
                    if tag_end < len && chars[tag_end] == '$' {
                        // Found opening $tag$ (or $$) — collect the tag string
                        let tag: String = chars[tag_start..=tag_end].iter().collect();
                        result.push_str(&tag);
                        i = tag_end + 1;
                        // Find closing $tag$
                        let rest: String = chars[i..].iter().collect();
                        if let Some(close_pos) = rest.find(&tag) {
                            result.push_str(&rest[..close_pos]);
                            result.push_str(&tag);
                            i += close_pos + tag.len();
                        } else {
                            // No closing tag — emit rest of SQL as-is
                            result.push_str(&rest);
                            i = len;
                        }
                        continue;
                    }
                    // Not a dollar quote — emit the $ and continue
                    result.push('$');
                    i += 1;
                    continue;
                }

                // Double-quoted identifier
                if ch == '"' {
                    result.push(ch);
                    i += 1;
                    state = State::InIdentifier;
                    continue;
                }

                // Single-quoted string literal
                if ch == '\'' {
                    string_buf.clear();
                    i += 1;
                    state = State::InStringLiteral;
                    continue;
                }

                // Numeric literal (digit not preceded by alphanumeric or underscore)
                if ch.is_ascii_digit() && !is_part_of_identifier(&result) {
                    state = State::InNumericLiteral;
                    // Don't advance i; we'll handle it in InNumericLiteral
                    continue;
                }

                // Operators that set eligibility
                if ch == '=' {
                    result.push(ch);
                    i += 1;
                    eligibility = Eligibility::Eligible;
                    continue;
                }
                if ch == '<' {
                    result.push(ch);
                    i += 1;
                    if i < len && (chars[i] == '>' || chars[i] == '=') {
                        result.push(chars[i]);
                        i += 1;
                    }
                    eligibility = Eligibility::Eligible;
                    continue;
                }
                if ch == '>' {
                    result.push(ch);
                    i += 1;
                    if i < len && chars[i] == '=' {
                        result.push(chars[i]);
                        i += 1;
                    }
                    eligibility = Eligibility::Eligible;
                    continue;
                }
                if ch == '!' && i + 1 < len && chars[i + 1] == '=' {
                    result.push('!');
                    result.push('=');
                    i += 2;
                    eligibility = Eligibility::Eligible;
                    continue;
                }

                // Alphabetic: could be a keyword (Unicode-aware for international identifiers)
                if ch.is_alphabetic() || ch == '_' {
                    let word_start = i;
                    while i < len && (chars[i].is_alphanumeric() || chars[i] == '_') {
                        result.push(chars[i]);
                        i += 1;
                    }
                    let word = &sql[word_start..word_start + (i - word_start)];
                    let upper = word.to_ascii_uppercase();

                    match upper.as_str() {
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
                            // If we're in identifier context, this word consumed it
                            if eligibility == Eligibility::IdentifierContext {
                                eligibility = Eligibility::Neutral;
                            }
                        }
                    }
                    continue;
                }

                // Parentheses and commas do NOT change eligibility
                // Everything else: pass through
                result.push(ch);
                i += 1;
            }

            State::InStringLiteral => {
                if chars[i] == '\'' {
                    if i + 1 < len && chars[i + 1] == '\'' {
                        // Escaped quote
                        string_buf.push('\'');
                        string_buf.push('\'');
                        i += 2;
                    } else {
                        // End of string literal
                        i += 1;
                        let should_substitute =
                            matches!(eligibility, Eligibility::Neutral | Eligibility::Eligible);

                        if should_substitute {
                            if let Some(replacement) = map.get(&string_buf) {
                                result.push('\'');
                                result.push_str(replacement.value());
                                result.push('\'');
                                count += 1;
                            } else {
                                result.push('\'');
                                result.push_str(&string_buf);
                                result.push('\'');
                            }
                        } else {
                            result.push('\'');
                            result.push_str(&string_buf);
                            result.push('\'');
                        }

                        // Reset eligibility after consuming a literal
                        if eligibility != Eligibility::Neutral {
                            eligibility = Eligibility::Neutral;
                        }
                        state = State::Normal;
                    }
                } else {
                    string_buf.push(chars[i]);
                    i += 1;
                }
            }

            State::InIdentifier => {
                result.push(chars[i]);
                if chars[i] == '"' {
                    i += 1;
                    state = State::Normal;
                    // Identifier context consumed
                    if eligibility == Eligibility::IdentifierContext {
                        eligibility = Eligibility::Neutral;
                    }
                } else {
                    i += 1;
                }
            }

            State::InLineComment => {
                result.push(chars[i]);
                if chars[i] == '\n' {
                    state = State::Normal;
                }
                i += 1;
            }

            State::InBlockComment => {
                result.push(chars[i]);
                if chars[i] == '*' && i + 1 < len && chars[i + 1] == '/' {
                    result.push('/');
                    i += 2;
                    state = State::Normal;
                } else {
                    i += 1;
                }
            }

            State::InNumericLiteral => {
                // Accumulate the full numeric literal
                let num_start = i;
                while i < len && chars[i].is_ascii_digit() {
                    i += 1;
                }
                // Check if followed by alphanumeric or underscore (part of identifier)
                // or if it contains a dot (decimal - not an ID)
                let is_standalone = if i < len {
                    !(chars[i].is_ascii_alphabetic() || chars[i] == '_' || chars[i] == '.')
                } else {
                    true
                };

                if is_standalone {
                    let num_str: String = chars[num_start..i].iter().collect();
                    let should_substitute =
                        matches!(eligibility, Eligibility::Neutral | Eligibility::Eligible);

                    if should_substitute {
                        if let Some(replacement) = map.get(&num_str) {
                            // Only substitute if the replacement looks like a valid numeric value
                            let is_safe = replacement.value().chars().all(|c| {
                                c.is_ascii_digit()
                                    || c == '.'
                                    || c == '-'
                                    || c == 'e'
                                    || c == 'E'
                                    || c == '+'
                            });
                            if is_safe {
                                result.push_str(replacement.value());
                                count += 1;
                            } else {
                                // Unsafe replacement value — skip substitution, emit original
                                result.push_str(&num_str);
                            }
                        } else {
                            result.push_str(&num_str);
                        }
                    } else {
                        result.push_str(&num_str);
                    }

                    // Reset eligibility after consuming a literal
                    if eligibility != Eligibility::Neutral {
                        eligibility = Eligibility::Neutral;
                    }
                } else {
                    // Part of identifier or decimal: push as-is and continue
                    result.extend(chars[num_start..i].iter());
                    // Continue consuming identifier chars
                    while i < len
                        && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '.')
                    {
                        result.push(chars[i]);
                        i += 1;
                    }
                }

                state = State::Normal;
            }
        }
    }

    if count == 0 {
        (Cow::Borrowed(sql), 0)
    } else {
        (Cow::Owned(result), count)
    }
}

/// Check if the last character in result is part of an identifier (letter, digit, underscore).
fn is_part_of_identifier(result: &str) -> bool {
    result
        .chars()
        .last()
        .map(|c| c.is_ascii_alphanumeric() || c == '_')
        .unwrap_or(false)
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
