//! Oracle AWR / `V$SQL` / `DBA_HIST_SQLTEXT` extract capture — the *easiest* Oracle source
//! to produce: a DBA runs one query and exports a CSV, then uploads it (pg-retest never
//! connects to Oracle). The CSV needs a `sql_text` column; optional `executions` and
//! `elapsed_us`/`avg_elapsed_us` columns add weighting/timing.
//!
//! Example producer query:
//! ```sql
//! SELECT sql_text, executions, elapsed_time/GREATEST(executions,1) AS elapsed_us
//! FROM v$sql WHERE parsing_schema_name = 'APP_OWNER' ORDER BY executions DESC;
//! ```
//!
//! **Fidelity tradeoff (honest):** an AWR/`V$SQL` extract is a *summary* — distinct SQL
//! shapes and how often they ran, NOT an ordered, bound statement stream. Oracle shares
//! cursors by replacing literals with binds, so much of this SQL is parameterized
//! (`WHERE id = :B1`) with **no bind values available** — those statements will be skipped
//! on replay (no binds to substitute). Use the 10046 SQL Trace source (`oracle-trace`) for
//! faithful OLTP replay; use this for breadth of SQL shapes and literal/reporting queries.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use tracing::debug;

use crate::profile::{Metadata, Query, QueryKind, Session, SourceDialect, WorkloadProfile};

pub struct OracleAwrCapture;

impl OracleAwrCapture {
    pub fn capture_from_file(&self, path: &Path, source_host: &str) -> Result<WorkloadProfile> {
        let file = std::fs::File::open(path)
            .with_context(|| format!("Failed to open Oracle SQL extract: {}", path.display()))?;
        self.capture_from_reader(file, source_host)
    }

    pub fn capture_from_reader(
        &self,
        reader: impl std::io::Read,
        source_host: &str,
    ) -> Result<WorkloadProfile> {
        let mut rdr = csv::ReaderBuilder::new()
            .flexible(true)
            .trim(csv::Trim::All)
            .from_reader(reader);

        let headers = rdr.headers().context("CSV has no header row")?.clone();
        let col = |names: &[&str]| -> Option<usize> {
            headers
                .iter()
                .position(|h| names.iter().any(|n| h.eq_ignore_ascii_case(n)))
        };
        let sql_col = col(&["sql_text", "sql text", "sqltext", "sql"])
            .ok_or_else(|| anyhow!("CSV needs a `sql_text` column (got: {:?})", headers))?;
        let elapsed_col = col(&["elapsed_us", "avg_elapsed_us", "avg_us", "elapsed"]);

        let mut queries: Vec<Query> = Vec::new();
        let mut offset: u64 = 0;
        for rec in rdr.records() {
            let rec = rec.context("malformed CSV row")?;
            let sql = rec.get(sql_col).unwrap_or("").trim().to_string();
            if sql.is_empty() {
                continue;
            }
            let duration_us = elapsed_col
                .and_then(|c| rec.get(c))
                .and_then(|s| s.trim().parse::<f64>().ok())
                .map(|f| f as u64)
                .unwrap_or(0);
            queries.push(Query {
                kind: QueryKind::from_sql(&sql),
                sql,
                start_offset_us: offset,
                duration_us,
                transaction_id: None,
                response_values: None,
                original_sql: None,
            });
            // AWR has no real ordering; lay statements out sequentially by duration.
            offset += duration_us.max(1);
        }

        debug!("Oracle AWR/V$SQL extract: {} statements", queries.len());
        let total_queries = queries.len() as u64;
        let sessions = if queries.is_empty() {
            Vec::new()
        } else {
            vec![Session {
                id: 1,
                user: source_host.to_string(),
                database: String::new(),
                queries,
            }]
        };
        let total_sessions = sessions.len() as u64;

        Ok(WorkloadProfile {
            version: 2,
            captured_at: Utc::now(),
            source_host: source_host.to_string(),
            pg_version: "unknown".to_string(),
            capture_method: "oracle_awr".to_string(),
            sessions,
            metadata: Metadata {
                total_queries,
                total_sessions,
                capture_duration_us: offset,
                sequence_snapshot: None,
                pk_map: None,
            },
            source_dialect: SourceDialect::Oracle,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const CSV: &str = "\
sql_text,executions,elapsed_us
\"SELECT NVL(name, 'x') FROM products\",100,1500
\"INSERT INTO events (note) VALUES ('awr_origin')\",5,800
";

    #[test]
    fn test_parses_sql_text_and_timing() {
        let p = OracleAwrCapture
            .capture_from_reader(Cursor::new(CSV), "orcl")
            .unwrap();
        assert_eq!(p.source_dialect, SourceDialect::Oracle);
        assert_eq!(p.capture_method, "oracle_awr");
        assert_eq!(p.metadata.total_queries, 2);
        let qs = &p.sessions[0].queries;
        assert!(qs[0].sql.contains("NVL"));
        assert_eq!(qs[0].duration_us, 1500);
        assert!(qs[1].sql.contains("awr_origin"));
    }

    #[test]
    fn test_missing_sql_text_column_errors() {
        let bad = "foo,bar\n1,2\n";
        assert!(OracleAwrCapture
            .capture_from_reader(Cursor::new(bad), "orcl")
            .is_err());
    }

    #[test]
    fn test_only_sql_text_column_ok() {
        let csv = "sql_text\n\"SELECT 1 FROM dual\"\n";
        let p = OracleAwrCapture
            .capture_from_reader(Cursor::new(csv), "orcl")
            .unwrap();
        assert_eq!(p.metadata.total_queries, 1);
        assert_eq!(p.sessions[0].queries[0].duration_us, 0);
    }
}
