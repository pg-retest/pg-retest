use serde::{Deserialize, Serialize};

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
