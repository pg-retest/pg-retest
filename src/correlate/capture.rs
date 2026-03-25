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
