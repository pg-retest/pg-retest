pub mod io;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::correlate::capture::{ResponseRow, TablePk};
use crate::correlate::sequence::SequenceState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkloadProfile {
    pub version: u8,
    pub captured_at: DateTime<Utc>,
    pub source_host: String,
    pub pg_version: String,
    pub capture_method: String,
    pub sessions: Vec<Session>,
    pub metadata: Metadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: u64,
    pub user: String,
    pub database: String,
    pub queries: Vec<Query>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Query {
    pub sql: String,
    pub start_offset_us: u64,
    pub duration_us: u64,
    pub kind: QueryKind,
    #[serde(default)]
    pub transaction_id: Option<u64>,
    #[serde(default)]
    pub response_values: Option<Vec<ResponseRow>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueryKind {
    Select,
    Insert,
    Update,
    Delete,
    Ddl,
    Begin,
    Commit,
    Rollback,
    Other,
}

impl QueryKind {
    pub fn from_sql(sql: &str) -> Self {
        let trimmed = sql.trim_start().to_uppercase();
        if trimmed.starts_with("SELECT") || trimmed.starts_with("WITH") {
            QueryKind::Select
        } else if trimmed.starts_with("INSERT") {
            QueryKind::Insert
        } else if trimmed.starts_with("UPDATE") {
            QueryKind::Update
        } else if trimmed.starts_with("DELETE") {
            QueryKind::Delete
        } else if trimmed.starts_with("CREATE")
            || trimmed.starts_with("ALTER")
            || trimmed.starts_with("DROP")
            || trimmed.starts_with("TRUNCATE")
        {
            QueryKind::Ddl
        } else if trimmed.starts_with("BEGIN") || trimmed.starts_with("START TRANSACTION") {
            QueryKind::Begin
        } else if trimmed.starts_with("COMMIT") || trimmed.starts_with("END") {
            QueryKind::Commit
        } else if trimmed.starts_with("ROLLBACK TO") {
            // ROLLBACK TO SAVEPOINT stays inside the transaction — not a real rollback
            QueryKind::Other
        } else if trimmed.starts_with("ROLLBACK") || trimmed.starts_with("ABORT") {
            QueryKind::Rollback
        } else {
            QueryKind::Other
        }
    }

    pub fn is_read_only(&self) -> bool {
        matches!(self, QueryKind::Select)
    }

    pub fn is_transaction_control(&self) -> bool {
        matches!(
            self,
            QueryKind::Begin | QueryKind::Commit | QueryKind::Rollback
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    pub total_queries: u64,
    pub total_sessions: u64,
    pub capture_duration_us: u64,
    #[serde(default)]
    pub sequence_snapshot: Option<Vec<SequenceState>>,
    #[serde(default)]
    pub pk_map: Option<Vec<TablePk>>,
}

/// Assign transaction IDs to queries within BEGIN/COMMIT|ROLLBACK blocks.
/// Used by both CSV log capture and proxy capture.
pub fn assign_transaction_ids(queries: &mut [Query], next_txn_id: &mut u64) {
    let mut current_txn: Option<u64> = None;

    for query in queries.iter_mut() {
        match query.kind {
            QueryKind::Begin => {
                let txn_id = *next_txn_id;
                *next_txn_id += 1;
                current_txn = Some(txn_id);
                query.transaction_id = Some(txn_id);
            }
            QueryKind::Commit | QueryKind::Rollback => {
                if let Some(txn_id) = current_txn {
                    query.transaction_id = Some(txn_id);
                }
                current_txn = None;
            }
            _ => {
                query.transaction_id = current_txn;
            }
        }
    }
}
