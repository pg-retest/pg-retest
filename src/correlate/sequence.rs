use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio_postgres::Client;
use tracing::{info, warn};

/// Snapshot of a single PostgreSQL sequence's state at capture time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SequenceState {
    pub schema: String,
    pub name: String,
    pub last_value: Option<i64>,
    pub increment_by: i64,
    pub start_value: i64,
    pub min_value: i64,
    pub max_value: i64,
    pub cycle: bool,
    pub is_called: bool,
}

impl SequenceState {
    /// Returns the schema-qualified name for use in SQL.
    pub fn qualified_name(&self) -> String {
        format!("\"{}\".\"{}\"", self.schema, self.name)
    }

    /// Generates the setval() SQL to restore this sequence on a target.
    pub fn restore_sql(&self) -> String {
        if self.is_called {
            if let Some(val) = self.last_value {
                format!("SELECT setval('{}', {}, true)", self.qualified_name(), val)
            } else {
                format!(
                    "SELECT setval('{}', {}, false)",
                    self.qualified_name(),
                    self.start_value
                )
            }
        } else {
            format!(
                "SELECT setval('{}', {}, false)",
                self.qualified_name(),
                self.start_value
            )
        }
    }
}

/// Snapshot all user-defined sequences from the source database.
pub async fn snapshot_sequences(client: &Client) -> Result<Vec<SequenceState>> {
    let rows = client
        .query(
            "SELECT schemaname, sequencename, last_value, increment_by, \
             start_value, min_value, max_value, cycle, \
             last_value IS NOT NULL AS is_called \
             FROM pg_sequences \
             WHERE schemaname NOT IN ('pg_catalog', 'information_schema')",
            &[],
        )
        .await
        .context("Failed to query pg_sequences for sequence snapshot")?;

    let mut sequences = Vec::with_capacity(rows.len());
    for row in &rows {
        sequences.push(SequenceState {
            schema: row.get("schemaname"),
            name: row.get("sequencename"),
            last_value: row.get("last_value"),
            increment_by: row.get("increment_by"),
            start_value: row.get("start_value"),
            min_value: row.get("min_value"),
            max_value: row.get("max_value"),
            cycle: row.get("cycle"),
            is_called: row.get("is_called"),
        });
    }
    info!("Sequence snapshot captured: {} sequences", sequences.len());
    Ok(sequences)
}

/// Restore sequences on the target database using setval().
pub async fn restore_sequences(
    client: &Client,
    snapshot: &[SequenceState],
) -> (usize, usize, usize) {
    let mut reset = 0usize;
    let mut skipped = 0usize;
    let mut errors = 0usize;

    for seq in snapshot {
        let sql = seq.restore_sql();
        match client.simple_query(&sql).await {
            Ok(_) => reset += 1,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("does not exist") {
                    warn!(
                        "Sequence {} not found on target — skipping.",
                        seq.qualified_name()
                    );
                    skipped += 1;
                } else {
                    warn!("Failed to reset sequence {}: {}", seq.qualified_name(), msg);
                    errors += 1;
                }
            }
        }
    }
    info!(
        "Sequence sync complete — {} reset, {} skipped, {} errors.",
        reset, skipped, errors
    );
    (reset, skipped, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qualified_name() {
        let s = SequenceState {
            schema: "public".into(),
            name: "orders_id_seq".into(),
            last_value: Some(42),
            increment_by: 1,
            start_value: 1,
            min_value: 1,
            max_value: i64::MAX,
            cycle: false,
            is_called: true,
        };
        assert_eq!(s.qualified_name(), r#""public"."orders_id_seq""#);
    }

    #[test]
    fn test_restore_sql_called() {
        let s = SequenceState {
            schema: "public".into(),
            name: "orders_id_seq".into(),
            last_value: Some(42),
            increment_by: 1,
            start_value: 1,
            min_value: 1,
            max_value: i64::MAX,
            cycle: false,
            is_called: true,
        };
        assert_eq!(
            s.restore_sql(),
            r#"SELECT setval('"public"."orders_id_seq"', 42, true)"#
        );
    }

    #[test]
    fn test_restore_sql_not_called() {
        let s = SequenceState {
            schema: "public".into(),
            name: "orders_id_seq".into(),
            last_value: None,
            increment_by: 1,
            start_value: 1,
            min_value: 1,
            max_value: i64::MAX,
            cycle: false,
            is_called: false,
        };
        assert_eq!(
            s.restore_sql(),
            r#"SELECT setval('"public"."orders_id_seq"', 1, false)"#
        );
    }

    #[test]
    fn test_sequence_state_roundtrip() {
        let s = SequenceState {
            schema: "myschema".into(),
            name: "counter_seq".into(),
            last_value: Some(99999),
            increment_by: 10,
            start_value: 100,
            min_value: 1,
            max_value: 999999,
            cycle: true,
            is_called: true,
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: SequenceState = serde_json::from_str(&json).unwrap();
        assert_eq!(back.schema, "myschema");
        assert_eq!(back.last_value, Some(99999));
        assert!(back.cycle);
    }
}
