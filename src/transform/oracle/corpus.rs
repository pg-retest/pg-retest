//! Golden corpus: MySQL queries paired with an author-verified correct PostgreSQL
//! translation. The oracle computes truth by running `correct_pg_sql` live on the
//! seeded PG — no stored result blobs to maintain.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    pub id: String,
    pub mysql_sql: String,
    /// The human-verified correct PostgreSQL translation — the `GoldenOracle`'s source of
    /// truth. Optional: the live (real-MySQL) oracle derives truth by execution, so a
    /// MySQL-only deep corpus omits it.
    #[serde(default)]
    pub correct_pg_sql: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Corpus {
    pub case: Vec<Case>,
}

impl Corpus {
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parses_corpus() {
        let s = r#"
            [[case]]
            id = "ifnull"
            mysql_sql = "SELECT IFNULL(a,b) FROM t"
            correct_pg_sql = "SELECT COALESCE(a,b) FROM t"
            note = "IFNULL->COALESCE"
        "#;
        let c = Corpus::from_toml(s).unwrap();
        assert_eq!(c.case.len(), 1);
        assert_eq!(c.case[0].id, "ifnull");
        assert!(c.case[0].correct_pg_sql.contains("COALESCE"));
    }
}
