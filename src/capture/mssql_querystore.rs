//! SQL Server Query Store extract capture — the SQL Server analog of the Oracle
//! AWR/`V$SQL` extract (`oracle_awr.rs`): a DBA runs one join query and exports a CSV,
//! then uploads it (pg-retest never connects to SQL Server). The CSV needs a
//! `query_sql_text` column; an optional `avg_duration`/`last_duration` column adds
//! timing — Query Store reports these in **microseconds** already (verified against
//! `sys.query_store_runtime_stats`, Microsoft Learn), so no unit conversion is applied.
//!
//! Example producer query (joins the four Query Store catalog views):
//! ```sql
//! SELECT qt.query_sql_text, rs.count_executions, rs.avg_duration
//! FROM sys.query_store_query_text qt
//! JOIN sys.query_store_query q ON q.query_text_id = qt.query_text_id
//! JOIN sys.query_store_plan p ON p.query_id = q.query_id
//! JOIN sys.query_store_runtime_stats rs ON rs.plan_id = p.plan_id
//! ORDER BY rs.count_executions DESC;
//! ```
//!
//! **Fidelity tradeoff (honest):** a Query Store extract is a *summary* — distinct
//! query shapes and their aggregate timing, NOT an ordered, bound statement stream.
//! Query Store normalizes literals into parameters, so most captured SQL is
//! parameterized (`WHERE id = @0`) with **no parameter values available** — those
//! statements will be skipped on replay (no values to substitute). Use the Profiler
//! trace-table source (`mssql-trace`) for faithful OLTP replay; use this for breadth
//! of SQL shapes and reporting/ad-hoc queries.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use tracing::debug;

use crate::profile::{Metadata, Query, QueryKind, Session, SourceDialect, WorkloadProfile};

pub struct MssqlQueryStoreCapture;

impl MssqlQueryStoreCapture {
    pub fn capture_from_file(&self, path: &Path, source_host: &str) -> Result<WorkloadProfile> {
        let file = std::fs::File::open(path)
            .with_context(|| format!("Failed to open Query Store extract: {}", path.display()))?;
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
        let sql_col = col(&["query_sql_text", "sql_text", "sql text", "sqltext", "sql"])
            .ok_or_else(|| anyhow!("CSV needs a `query_sql_text` column (got: {:?})", headers))?;
        let duration_col = col(&["avg_duration", "last_duration", "duration_us", "duration"]);

        let mut queries: Vec<Query> = Vec::new();
        let mut offset: u64 = 0;
        for rec in rdr.records() {
            let rec = rec.context("malformed CSV row")?;
            let sql = rec.get(sql_col).unwrap_or("").trim().to_string();
            if sql.is_empty() {
                continue;
            }
            let duration_us = duration_col
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
            // Query Store has no real ordering; lay statements out sequentially by duration.
            offset += duration_us.max(1);
        }

        debug!(
            "SQL Server Query Store extract: {} statements",
            queries.len()
        );
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
            capture_method: "mssql_querystore".to_string(),
            sessions,
            metadata: Metadata {
                total_queries,
                total_sessions,
                capture_duration_us: offset,
                sequence_snapshot: None,
                pk_map: None,
            },
            source_dialect: SourceDialect::SqlServer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const CSV: &str = "\
query_sql_text,count_executions,avg_duration
\"SELECT TOP 10 name FROM products\",100,1500
\"INSERT INTO events (note) VALUES ('qs_origin')\",5,800
";

    #[test]
    fn test_parses_sql_text_and_timing() {
        let p = MssqlQueryStoreCapture
            .capture_from_reader(Cursor::new(CSV), "mssql01")
            .unwrap();
        assert_eq!(p.source_dialect, SourceDialect::SqlServer);
        assert_eq!(p.capture_method, "mssql_querystore");
        assert_eq!(p.metadata.total_queries, 2);
        let qs = &p.sessions[0].queries;
        assert!(qs[0].sql.contains("TOP 10"));
        assert_eq!(qs[0].duration_us, 1500);
        assert!(qs[1].sql.contains("qs_origin"));
    }

    #[test]
    fn test_missing_sql_column_errors() {
        let bad = "foo,bar\n1,2\n";
        let err = MssqlQueryStoreCapture
            .capture_from_reader(Cursor::new(bad), "mssql01")
            .unwrap_err();
        assert!(err.to_string().contains("query_sql_text"));
    }

    #[test]
    fn test_empty_sql_rows_skipped() {
        let csv = "query_sql_text,avg_duration\n,100\n\"SELECT 1\",200\n";
        let p = MssqlQueryStoreCapture
            .capture_from_reader(Cursor::new(csv), "mssql01")
            .unwrap();
        assert_eq!(p.metadata.total_queries, 1);
    }
}
