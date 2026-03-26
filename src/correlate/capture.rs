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

/// If SQL is a bare INSERT (no RETURNING) targeting a known PK table, return modified SQL with RETURNING appended.
pub fn inject_returning(sql: &str, pk_map: &[TablePk]) -> Option<String> {
    if has_returning(sql) {
        return None;
    }
    let upper = sql.trim_start().to_uppercase();
    if !upper.starts_with("INSERT") {
        return None;
    }

    // Extract table name after INTO
    let into_pos = upper.find("INTO ")?;
    let after_into = sql[into_pos + 5..].trim_start();
    let table_name: String = after_into
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.' || *c == '"')
        .collect();

    let table_clean = table_name.replace('"', "");
    let pk = pk_map.iter().find(|pk| {
        let qualified = format!("{}.{}", pk.schema, pk.table);
        table_clean == pk.table || table_clean == qualified
    })?;

    let returning_cols = pk.columns.join(", ");
    let trimmed = sql.trim_end().trim_end_matches(';');

    // RETURNING must come BEFORE ON CONFLICT if present
    let upper_trimmed = trimmed.to_uppercase();
    if let Some(conflict_pos) = upper_trimmed.find(" ON CONFLICT") {
        let before_conflict = &trimmed[..conflict_pos];
        let conflict_clause = &trimmed[conflict_pos..];
        Some(format!(
            "{} RETURNING {}{}",
            before_conflict, returning_cols, conflict_clause
        ))
    } else {
        Some(format!("{} RETURNING {}", trimmed, returning_cols))
    }
}

/// Detect if a query is SELECT currval(...) or SELECT lastval().
pub fn is_currval_or_lastval(sql: &str) -> bool {
    let upper = sql.trim_start().to_uppercase();
    upper.starts_with("SELECT") && (upper.contains("CURRVAL") || upper.contains("LASTVAL"))
}

/// Discover primary key columns for all user tables.
pub async fn discover_primary_keys(
    client: &tokio_postgres::Client,
) -> anyhow::Result<Vec<TablePk>> {
    use anyhow::Context;
    let rows = client
        .query(
            "SELECT kcu.table_schema, kcu.table_name, kcu.column_name, kcu.ordinal_position \
             FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu \
                 USING (constraint_schema, constraint_name, table_schema, table_name) \
             WHERE tc.constraint_type = 'PRIMARY KEY' \
                 AND tc.table_schema NOT IN ('pg_catalog', 'information_schema') \
             ORDER BY kcu.table_schema, kcu.table_name, kcu.ordinal_position",
            &[],
        )
        .await
        .context("Failed to discover primary keys")?;

    let mut pk_map: std::collections::BTreeMap<(String, String), Vec<String>> =
        std::collections::BTreeMap::new();
    for row in &rows {
        let schema: String = row.get(0);
        let table: String = row.get(1);
        let column: String = row.get(2);
        pk_map.entry((schema, table)).or_default().push(column);
    }

    Ok(pk_map
        .into_iter()
        .map(|((schema, table), columns)| TablePk {
            schema,
            table,
            columns,
        })
        .collect())
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

    #[test]
    fn test_inject_returning() {
        let pk_map = vec![TablePk {
            schema: "public".into(),
            table: "orders".into(),
            columns: vec!["id".into()],
        }];
        assert_eq!(
            inject_returning("INSERT INTO orders (name) VALUES ('test')", &pk_map),
            Some("INSERT INTO orders (name) VALUES ('test') RETURNING id".into())
        );
    }

    #[test]
    fn test_inject_returning_already_has() {
        let pk_map = vec![TablePk {
            schema: "public".into(),
            table: "orders".into(),
            columns: vec!["id".into()],
        }];
        assert!(inject_returning(
            "INSERT INTO orders (name) VALUES ('test') RETURNING id",
            &pk_map
        )
        .is_none());
    }

    #[test]
    fn test_inject_returning_unknown_table() {
        let pk_map = vec![TablePk {
            schema: "public".into(),
            table: "orders".into(),
            columns: vec!["id".into()],
        }];
        assert!(inject_returning("INSERT INTO unknown (name) VALUES ('test')", &pk_map).is_none());
    }

    #[test]
    fn test_detect_currval() {
        assert!(is_currval_or_lastval("SELECT currval('orders_id_seq')"));
        assert!(is_currval_or_lastval("SELECT lastval()"));
        assert!(!is_currval_or_lastval("SELECT * FROM orders"));
    }

    #[test]
    fn test_inject_returning_on_conflict() {
        let pk_map = vec![TablePk {
            schema: "public".into(),
            table: "orders".into(),
            columns: vec!["id".into()],
        }];
        // RETURNING must come BEFORE ON CONFLICT
        assert_eq!(
            inject_returning(
                "INSERT INTO orders (id, name) VALUES (1, 'test') ON CONFLICT DO NOTHING",
                &pk_map
            ),
            Some(
                "INSERT INTO orders (id, name) VALUES (1, 'test') RETURNING id ON CONFLICT DO NOTHING"
                    .into()
            )
        );
    }

    #[test]
    fn test_inject_returning_on_conflict_do_update() {
        let pk_map = vec![TablePk {
            schema: "public".into(),
            table: "orders".into(),
            columns: vec!["id".into()],
        }];
        assert_eq!(
            inject_returning(
                "INSERT INTO orders (id, name) VALUES (1, 'test') ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name",
                &pk_map
            ),
            Some(
                "INSERT INTO orders (id, name) VALUES (1, 'test') RETURNING id ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name"
                    .into()
            )
        );
    }

    #[test]
    fn test_inject_returning_empty_sql() {
        let pk_map = vec![TablePk {
            schema: "public".into(),
            table: "orders".into(),
            columns: vec!["id".into()],
        }];
        assert!(inject_returning("", &pk_map).is_none());
    }

    #[test]
    fn test_inject_returning_insert_select() {
        let pk_map = vec![TablePk {
            schema: "public".into(),
            table: "orders".into(),
            columns: vec!["id".into()],
        }];
        assert_eq!(
            inject_returning(
                "INSERT INTO orders (id, name) SELECT id, name FROM staging",
                &pk_map
            ),
            Some("INSERT INTO orders (id, name) SELECT id, name FROM staging RETURNING id".into())
        );
    }
}
