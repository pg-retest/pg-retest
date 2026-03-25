use serde::{Deserialize, Serialize};

/// A single row of captured RETURNING clause results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseRow {
    /// (column_name, text_value) pairs from the RETURNING result.
    pub columns: Vec<(String, String)>,
}

/// Primary key column mapping for a table, used by `--id-capture-implicit`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TablePk {
    pub schema: String,
    pub table: String,
    /// PK column names in ordinal order.
    pub columns: Vec<String>,
}

/// Check if SQL contains a RETURNING clause (not inside a string literal).
pub fn has_returning(sql: &str) -> bool {
    let upper = sql.to_uppercase();
    if !upper.contains("RETURNING") {
        return false;
    }
    let mut in_string = false;
    let chars: Vec<char> = sql.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\'' {
            if in_string && i + 1 < chars.len() && chars[i + 1] == '\'' {
                i += 2;
                continue;
            }
            in_string = !in_string;
        } else if !in_string && i + 9 <= chars.len() {
            let chunk: String = chars[i..i + 9].iter().collect();
            if chunk.eq_ignore_ascii_case("RETURNING") {
                let before_ok = i == 0 || !chars[i - 1].is_alphanumeric();
                let after_ok = i + 9 >= chars.len() || !chars[i + 9].is_alphanumeric();
                if before_ok && after_ok {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_has_returning_simple() {
        assert!(has_returning("INSERT INTO t (a) VALUES (1) RETURNING id"));
    }

    #[test]
    fn test_has_returning_case_insensitive() {
        assert!(has_returning("INSERT INTO t VALUES (1) returning id"));
        assert!(has_returning("INSERT INTO t VALUES (1) Returning id, name"));
    }

    #[test]
    fn test_has_returning_absent() {
        assert!(!has_returning("INSERT INTO t VALUES (1)"));
        assert!(!has_returning("SELECT * FROM t"));
        assert!(!has_returning("UPDATE t SET a = 1"));
    }

    #[test]
    fn test_has_returning_inside_string_literal() {
        assert!(!has_returning("INSERT INTO t (a) VALUES ('RETURNING id')"));
        assert!(!has_returning(
            "INSERT INTO t (a) VALUES ('has RETURNING clause')"
        ));
    }
}
