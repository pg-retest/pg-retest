//! SQL Server Profiler trace-table capture — the SQL Server analog of the Oracle SQL
//! Trace parser (`oracle_trace.rs`). Raw binary `.trc` files can't be parsed directly
//! (proprietary format, no public spec); the standard workaround — used by every
//! third-party tool in this space — is to load the trace into a table with
//! `SELECT * INTO my_trace FROM ::fn_trace_gettable('trace.trc', default)` (or
//! Profiler's "Save As > Trace Table"), then export that table to CSV. pg-retest
//! never connects to SQL Server: the DBA does this offline and uploads the CSV.
//!
//! Required column: `TextData` (the SQL command text). Optional columns improve
//! fidelity: `Duration` (**microseconds** — verified against Microsoft's trace-column
//! docs: Profiler's GUI displays milliseconds, but the value written to a table or
//! file is always microseconds), `StartTime` (real per-session ordering), `SPID`
//! (session grouping — SQL Server's connection/session identifier), `DatabaseName`,
//! `LoginName`. Column names are matched case-insensitively; all are optional except
//! `TextData`.
//!
//! If an `EventClass` column is present, rows are filtered to completed batches/RPCs
//! only (`SQL:BatchCompleted` = 12, `RPC:Completed` = 10 — the two standard "this
//! statement finished, here's its total duration" event classes). Without that
//! column, every row is accepted as-is (assumes the DBA pre-filtered the export).
//!
//! **Fidelity tradeoff (honest):** Profiler traces capture literal SQL text (no bind
//! placeholders to worry about, unlike Oracle's shared-cursor model), so this source
//! is suitable for faithful OLTP replay when `StartTime`/`SPID` are present. Without
//! them, statements land in one flat session in file order — see the AWR-style
//! Query Store extract (`mssql-querystore`) for that same summary-only tradeoff.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, NaiveDateTime, Utc};
use tracing::debug;

use crate::profile::{
    assign_transaction_ids, Metadata, Query, QueryKind, Session, SourceDialect, WorkloadProfile,
};

pub struct MssqlTraceCapture;

struct TraceRow {
    spid: u64,
    database: String,
    login: String,
    timestamp: Option<DateTime<Utc>>,
    duration_us: u64,
    sql: String,
}

impl MssqlTraceCapture {
    pub fn capture_from_file(&self, path: &Path, source_host: &str) -> Result<WorkloadProfile> {
        let file = std::fs::File::open(path).with_context(|| {
            format!("Failed to open SQL Server trace export: {}", path.display())
        })?;
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
        let text_col = col(&["textdata", "text_data", "sql_text", "sqltext"])
            .ok_or_else(|| anyhow!("CSV needs a `TextData` column (got: {:?})", headers))?;
        let duration_col = col(&["duration", "duration_us"]);
        let start_time_col = col(&["starttime", "start_time"]);
        let spid_col = col(&["spid", "session_id"]);
        let database_col = col(&["databasename", "database_name"]);
        let login_col = col(&["loginname", "login_name"]);
        let event_class_col = col(&["eventclass", "event_class"]);

        let mut rows: Vec<TraceRow> = Vec::new();
        for rec in rdr.records() {
            let rec = rec.context("malformed CSV row")?;

            if let Some(c) = event_class_col {
                let v = rec.get(c).unwrap_or("").trim();
                let is_completed = v == "10"
                    || v == "12"
                    || v.eq_ignore_ascii_case("RPC:Completed")
                    || v.eq_ignore_ascii_case("SQL:BatchCompleted");
                if !is_completed {
                    continue;
                }
            }

            let sql = rec.get(text_col).unwrap_or("").trim().to_string();
            if sql.is_empty() {
                continue;
            }

            let duration_us = duration_col
                .and_then(|c| rec.get(c))
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(0);
            let spid = spid_col
                .and_then(|c| rec.get(c))
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(0);
            let timestamp = start_time_col
                .and_then(|c| rec.get(c))
                .and_then(parse_mssql_timestamp);

            rows.push(TraceRow {
                spid,
                database: database_col
                    .and_then(|c| rec.get(c))
                    .unwrap_or("")
                    .to_string(),
                login: login_col.and_then(|c| rec.get(c)).unwrap_or("").to_string(),
                timestamp,
                duration_us,
                sql,
            });
        }

        debug!(
            "SQL Server trace export: {} completed statements",
            rows.len()
        );

        // Group by SPID (SQL Server's session identifier), same shape as MySQL slow-log's
        // thread_id grouping.
        let mut session_map: HashMap<u64, Vec<TraceRow>> = HashMap::new();
        for row in rows {
            session_map.entry(row.spid).or_default().push(row);
        }

        let mut sessions = Vec::new();
        let mut total_queries: u64 = 0;
        let mut next_txn_id: u64 = 1;
        let mut global_min: Option<DateTime<Utc>> = None;
        let mut global_max: Option<DateTime<Utc>> = None;

        for (spid, mut trace_rows) in session_map {
            if trace_rows.is_empty() {
                continue;
            }
            trace_rows.sort_by_key(|r| r.timestamp);

            let first_time = trace_rows[0].timestamp;
            let login = trace_rows[0].login.clone();
            let database = trace_rows[0].database.clone();

            for r in &trace_rows {
                if let Some(t) = r.timestamp {
                    global_min = Some(global_min.map_or(t, |m| m.min(t)));
                    global_max = Some(global_max.map_or(t, |m| m.max(t)));
                }
            }

            let mut queries: Vec<Query> = Vec::new();
            for row in trace_rows {
                let offset = match (row.timestamp, first_time) {
                    (Some(t), Some(first)) => (t - first).num_microseconds().unwrap_or(0) as u64,
                    _ => 0,
                };
                queries.push(Query {
                    kind: QueryKind::from_sql(&row.sql),
                    sql: row.sql,
                    start_offset_us: offset,
                    duration_us: row.duration_us,
                    transaction_id: None,
                    response_values: None,
                    original_sql: None,
                });
            }

            assign_transaction_ids(&mut queries, &mut next_txn_id);
            total_queries += queries.len() as u64;

            if !queries.is_empty() {
                sessions.push(Session {
                    id: spid,
                    user: login,
                    database,
                    queries,
                });
            }
        }

        sessions.sort_by_key(|s| s.id);

        let capture_duration_us = match (global_min, global_max) {
            (Some(min), Some(max)) => (max - min).num_microseconds().unwrap_or(0) as u64,
            _ => 0,
        };
        let total_sessions = sessions.len() as u64;

        Ok(WorkloadProfile {
            version: 2,
            captured_at: Utc::now(),
            source_host: source_host.to_string(),
            pg_version: "unknown".to_string(),
            capture_method: "mssql_trace".to_string(),
            sessions,
            metadata: Metadata {
                total_queries,
                total_sessions,
                capture_duration_us,
                sequence_snapshot: None,
                pk_map: None,
            },
            source_dialect: SourceDialect::SqlServer,
        })
    }
}

/// Parse a SQL Server trace `StartTime` value: typically `YYYY-MM-DD HH:MM:SS.mmm`
/// (the default string form when a datetime column is exported to CSV), with an
/// ISO 8601 fallback for exports that already normalize it.
fn parse_mssql_timestamp(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    if let Ok(dt) = s.parse::<DateTime<Utc>>() {
        return Some(dt);
    }
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f")
        .ok()
        .map(|naive| naive.and_utc())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const CSV: &str = "\
TextData,Duration,StartTime,SPID,DatabaseName,LoginName,EventClass
\"SELECT * FROM products WHERE id = 1\",1500,2026-03-08 10:00:00.100,52,app,svc_app,12
\"EXEC dbo.get_order 42\",800,2026-03-08 10:00:00.900,52,app,svc_app,10
\"SELECT 1\",100,2026-03-08 10:00:01.000,53,app,svc_app,12
\"sp_reset_connection\",5,2026-03-08 10:00:01.100,53,app,svc_app,14
";

    #[test]
    fn test_groups_by_spid_and_filters_event_class() {
        let p = MssqlTraceCapture
            .capture_from_reader(Cursor::new(CSV), "mssql01")
            .unwrap();
        assert_eq!(p.source_dialect, SourceDialect::SqlServer);
        assert_eq!(p.capture_method, "mssql_trace");
        // The EventClass=14 row (sp_reset_connection) is filtered out.
        assert_eq!(p.metadata.total_queries, 3);
        assert_eq!(p.metadata.total_sessions, 2);

        let spid_52 = p.sessions.iter().find(|s| s.id == 52).unwrap();
        assert_eq!(spid_52.queries.len(), 2);
        assert_eq!(spid_52.user, "svc_app");
        assert_eq!(spid_52.database, "app");
        // Second query is 800us after the first (10:00:00.900 - 10:00:00.100).
        assert_eq!(spid_52.queries[1].start_offset_us, 800_000);
        assert_eq!(spid_52.queries[1].duration_us, 800);

        let spid_53 = p.sessions.iter().find(|s| s.id == 53).unwrap();
        assert_eq!(spid_53.queries.len(), 1);
    }

    #[test]
    fn test_missing_text_data_column_errors() {
        let bad = "foo,bar\n1,2\n";
        let err = MssqlTraceCapture
            .capture_from_reader(Cursor::new(bad), "mssql01")
            .unwrap_err();
        assert!(err.to_string().contains("TextData"));
    }

    #[test]
    fn test_no_event_class_column_accepts_all_rows() {
        let csv = "TextData,Duration\n\"SELECT 1\",100\n\"sp_reset_connection\",5\n";
        let p = MssqlTraceCapture
            .capture_from_reader(Cursor::new(csv), "mssql01")
            .unwrap();
        assert_eq!(p.metadata.total_queries, 2);
    }
}
