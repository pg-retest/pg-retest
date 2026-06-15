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
}
