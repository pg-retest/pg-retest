//! Oracle SQL Trace (event 10046 raw trace) capture parser — the Oracle analog of the
//! MySQL slow-log parser. Parses an uploaded `.trc` file into a workload profile
//! (`source_dialect = Oracle`), ready for `oracle-replay` translation to PostgreSQL.
//! pg-retest never connects to Oracle: the DBA enables tracing, collects the `.trc`, and
//! uploads it (capture is decoupled from replay).
//!
//! Scope (v1):
//! - Top-level statements only: recursive/internal data-dictionary SQL (`dep>0`) is
//!   filtered out.
//! - One trace file = one session; `EXEC` events drive the statement stream
//!   (`e=` elapsed µs = duration, `tim=` µs = ordering). A re-executed cursor emits
//!   multiple statements (workload repetition).
//! - Bind-value substitution from `BINDS` sections is a follow-on; SQL with `:N` bind
//!   placeholders is captured as-is (translation/replay may skip what it can't bind).
//!
//! Format reference: Oracle SQL Trace / event 10046 raw trace (`PARSING IN CURSOR #n …
//! dep=d … tim=t` + SQL text + `END OF STMT`, then `EXEC #n:…e=…,tim=…`).

use std::collections::HashMap;
use std::io::BufRead;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use regex::Regex;
use tracing::debug;

use crate::profile::{
    assign_transaction_ids, Metadata, Query, QueryKind, Session, SourceDialect, WorkloadProfile,
};

pub struct OracleTraceCapture;

struct CursorSql {
    sql: String,
    dep: u32,
}

impl OracleTraceCapture {
    pub fn capture_from_file(&self, path: &Path, source_host: &str) -> Result<WorkloadProfile> {
        let file = std::fs::File::open(path)
            .with_context(|| format!("Failed to open Oracle trace: {}", path.display()))?;
        self.capture_from_reader(std::io::BufReader::new(file), source_host)
    }

    /// Core parser (reader-based for testability).
    pub fn capture_from_reader(
        &self,
        reader: impl BufRead,
        source_host: &str,
    ) -> Result<WorkloadProfile> {
        let re_parsing = Regex::new(r"^PARSING IN CURSOR #(\d+) ").unwrap();
        let re_exec = Regex::new(r"^EXEC #(\d+):").unwrap();
        let re_binds = Regex::new(r"^BINDS #(\d+):").unwrap();
        let re_value = Regex::new(r"^\s*value=(.*)").unwrap();
        let re_dep = Regex::new(r"\bdep=(\d+)").unwrap();
        let re_e = Regex::new(r"\be=(\d+)").unwrap();
        let re_tim = Regex::new(r"\btim=(\d+)").unwrap();

        // A line-indexed pass so multi-line blocks (SQL between PARSING/END OF STMT, and
        // BINDS sections) are easy to consume.
        let lines: Vec<String> = reader
            .lines()
            .collect::<std::io::Result<_>>()
            .context("Failed to read trace")?;

        let mut cursors: HashMap<String, CursorSql> = HashMap::new();
        // Most-recent bind values per cursor (positional, in Bind# order).
        let mut binds: HashMap<String, Vec<String>> = HashMap::new();
        let mut queries: Vec<Query> = Vec::new();
        let mut first_tim: Option<u64> = None;

        let mut i = 0;
        while i < lines.len() {
            let line = &lines[i];

            // PARSING IN CURSOR #n … → SQL text until END OF STMT.
            if let Some(caps) = re_parsing.captures(line) {
                let cursor = caps[1].to_string();
                let dep = re_dep
                    .captures(line)
                    .and_then(|c| c[1].parse().ok())
                    .unwrap_or(0);
                i += 1;
                let mut sql_lines = Vec::new();
                while i < lines.len() && !lines[i].starts_with("END OF STMT") {
                    sql_lines.push(lines[i].as_str());
                    i += 1;
                }
                cursors.insert(
                    cursor,
                    CursorSql {
                        sql: sql_lines.join(" ").trim().to_string(),
                        dep,
                    },
                );
                i += 1; // skip END OF STMT
                continue;
            }

            // BINDS #n: → collect each Bind#'s `value=` until the next trace event.
            if let Some(caps) = re_binds.captures(line) {
                let cursor = caps[1].to_string();
                let mut vals = Vec::new();
                i += 1;
                while i < lines.len() && !is_trace_event(&lines[i]) {
                    if let Some(v) = re_value.captures(&lines[i]) {
                        vals.push(v[1].trim().to_string());
                    }
                    i += 1;
                }
                binds.insert(cursor, vals);
                continue; // the boundary line is processed on the next iteration
            }

            // EXEC #n: → emit a (top-level) statement, substituting binds if present.
            if let Some(caps) = re_exec.captures(line) {
                let cursor = &caps[1];
                if let Some(c) = cursors.get(cursor) {
                    if c.dep == 0 && !c.sql.is_empty() {
                        let sql = match binds.get(cursor) {
                            Some(v) if !v.is_empty() => substitute_binds(&c.sql, v),
                            _ => c.sql.clone(),
                        };
                        let elapsed = re_e
                            .captures(line)
                            .and_then(|m| m[1].parse::<u64>().ok())
                            .unwrap_or(0);
                        let tim = re_tim
                            .captures(line)
                            .and_then(|m| m[1].parse::<u64>().ok())
                            .unwrap_or(0);
                        if first_tim.is_none() {
                            first_tim = Some(tim);
                        }
                        let offset = tim.saturating_sub(first_tim.unwrap_or(tim));
                        queries.push(Query {
                            kind: QueryKind::from_sql(&sql),
                            sql,
                            start_offset_us: offset,
                            duration_us: elapsed,
                            transaction_id: None,
                            response_values: None,
                            original_sql: None,
                        });
                    }
                }
            }
            i += 1;
        }

        let mut next_txn_id = 1;
        assign_transaction_ids(&mut queries, &mut next_txn_id);
        let total_queries = queries.len() as u64;
        let capture_duration_us = queries
            .last()
            .map(|q| q.start_offset_us + q.duration_us)
            .unwrap_or(0);
        debug!("Oracle trace: {total_queries} top-level statements captured");

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
            capture_method: "oracle_trace".to_string(),
            sessions,
            metadata: Metadata {
                total_queries,
                total_sessions,
                capture_duration_us,
                sequence_snapshot: None,
                pk_map: None,
            },
            source_dialect: SourceDialect::Oracle,
        })
    }
}

/// True if `line` begins a 10046 trace event (used to terminate a BINDS section).
fn is_trace_event(line: &str) -> bool {
    const EVENTS: &[&str] = &[
        "EXEC ",
        "PARSE ",
        "FETCH ",
        "CLOSE ",
        "STAT ",
        "WAIT ",
        "BINDS ",
        "PARSING IN CURSOR",
        "===",
        "***",
        "XCTEND",
    ];
    EVENTS.iter().any(|e| line.starts_with(e))
}

/// Substitute Oracle bind placeholders (`:1`, `:name`, …) positionally with their trace
/// values. Trace string values are double-quoted (`value="x"`) → SQL single-quoted
/// literals; numeric values pass through. Surplus placeholders (more than we have values
/// for) are left as-is, so the statement is later skipped rather than mistranslated.
///
/// Heuristic: matches `:\w+` outside of lexical awareness, so a `:NN` inside a string
/// literal (e.g. a `'10:30'` time) could be misread — bind substitution from raw traces
/// is a best-effort convenience; the behavioral oracle still gates the result.
fn substitute_binds(sql: &str, vals: &[String]) -> String {
    let re_bind = Regex::new(r":\w+").unwrap();
    let mut idx = 0;
    re_bind
        .replace_all(sql, |caps: &regex::Captures| {
            let out = if idx < vals.len() {
                format_bind(&vals[idx])
            } else {
                caps[0].to_string()
            };
            idx += 1;
            out
        })
        .into_owned()
}

/// Format one trace bind value as a SQL literal.
fn format_bind(v: &str) -> String {
    let t = v.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        format!("'{}'", t[1..t.len() - 1].replace('\'', "''")) // string literal
    } else {
        t.to_string() // number / raw
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const TRACE: &str = "\
*** SESSION ID:(123.456) 2026-06-15T10:00:00.000
PARSING IN CURSOR #140001 len=58 dep=0 uid=84 oct=3 lid=84 tim=1000000 hv=1 ad='0' sqlid='aaa'
SELECT NVL(name, 'none') AS n FROM employees WHERE active = 1
END OF STMT
PARSE #140001:c=100,e=200,p=0,cr=0,cu=0,mis=1,r=0,dep=0,og=1,plh=0,tim=1000000
EXEC #140001:c=300,e=1500,p=0,cr=0,cu=0,mis=0,r=0,dep=0,og=1,plh=1,tim=1000300
FETCH #140001:c=200,e=800,p=0,cr=5,cu=0,mis=0,r=2,dep=0,og=1,plh=1,tim=1001000
PARSING IN CURSOR #140002 len=40 dep=1 uid=0 oct=3 lid=0 tim=1002000 hv=2 ad='0' sqlid='bbb'
select obj# from obj$ where name=:1
END OF STMT
EXEC #140002:c=50,e=100,p=0,cr=2,cu=0,mis=0,r=0,dep=1,og=4,plh=2,tim=1002100
PARSING IN CURSOR #140003 len=52 dep=0 uid=84 oct=2 lid=84 tim=1003000 hv=3 ad='0' sqlid='ccc'
INSERT INTO audit_log (msg) VALUES ('oracle trace')
END OF STMT
EXEC #140003:c=400,e=2000,p=0,cr=1,cu=3,mis=0,r=1,dep=0,og=1,plh=3,tim=1003500
";

    #[test]
    fn test_parses_top_level_and_filters_recursive() {
        let profile = OracleTraceCapture
            .capture_from_reader(Cursor::new(TRACE), "orcl")
            .unwrap();

        assert_eq!(profile.source_dialect, SourceDialect::Oracle);
        assert_eq!(profile.capture_method, "oracle_trace");
        // The dep=1 recursive obj$ query is filtered; 2 top-level statements remain.
        assert_eq!(profile.metadata.total_queries, 2);
        let qs = &profile.sessions[0].queries;
        assert!(qs[0].sql.contains("NVL"));
        assert_eq!(qs[0].duration_us, 1500); // EXEC e=
        assert_eq!(qs[0].start_offset_us, 0); // first EXEC tim
        assert!(qs[1].sql.contains("INSERT INTO audit_log"));
        assert_eq!(qs[1].start_offset_us, 3200); // tim 1003500 - 1000300
        assert!(!qs.iter().any(|q| q.sql.contains("obj$"))); // recursive filtered
    }

    const BIND_TRACE: &str = "\
PARSING IN CURSOR #140005 len=46 dep=0 uid=84 oct=6 lid=84 tim=2000000 hv=5 ad='0' sqlid='upd'
UPDATE products SET price = :1 WHERE name = :2
END OF STMT
PARSE #140005:c=10,e=20,p=0,cr=0,cu=0,mis=1,r=0,dep=0,og=1,plh=0,tim=2000000
BINDS #140005:
 Bind#0
  oacdty=02 mxl=22(22) mxlc=00 mal=00 scl=00 pre=00
  value=99
 Bind#1
  oacdty=01 mxl=32(20) mxlc=00 mal=00 scl=00 pre=00
  value=\"alice\"
EXEC #140005:c=100,e=500,p=0,cr=0,cu=1,mis=0,r=1,dep=0,og=1,plh=5,tim=2000100
";

    #[test]
    fn test_bind_substitution() {
        let profile = OracleTraceCapture
            .capture_from_reader(Cursor::new(BIND_TRACE), "orcl")
            .unwrap();
        assert_eq!(profile.metadata.total_queries, 1);
        // :1 → 99 (number), :2 → 'alice' (string).
        assert_eq!(
            profile.sessions[0].queries[0].sql,
            "UPDATE products SET price = 99 WHERE name = 'alice'"
        );
    }

    #[test]
    fn test_format_bind_string_and_number() {
        assert_eq!(format_bind("99"), "99");
        assert_eq!(format_bind("\"alice\""), "'alice'");
        assert_eq!(format_bind("\"O'Brien\""), "'O''Brien'"); // quote doubling
    }

    #[test]
    fn test_empty_trace_yields_no_sessions() {
        let profile = OracleTraceCapture
            .capture_from_reader(Cursor::new("Trace file header\n"), "orcl")
            .unwrap();
        assert_eq!(profile.metadata.total_queries, 0);
        assert!(profile.sessions.is_empty());
    }
}
