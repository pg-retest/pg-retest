use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

use crate::profile::{Metadata, Query, QueryKind, Session, WorkloadProfile};

pub struct CsvLogCapture;

/// A raw parsed log entry before grouping into sessions.
struct LogEntry {
    log_time: DateTime<Utc>,
    user_name: String,
    database_name: String,
    session_id: String,
    duration_us: u64,
    sql: String,
}

impl CsvLogCapture {
    pub fn capture_from_file(
        &self,
        path: &Path,
        source_host: &str,
        pg_version: &str,
    ) -> Result<WorkloadProfile> {
        let entries = self.parse_csv(path)?;
        self.build_profile(entries, source_host, pg_version)
    }

    fn parse_csv(&self, path: &Path) -> Result<Vec<LogEntry>> {
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .from_path(path)
            .with_context(|| format!("Failed to open CSV log: {}", path.display()))?;

        let mut entries = Vec::new();

        for result in reader.records() {
            let record = result.context("Failed to read CSV record")?;

            // PG CSV log fields (0-indexed):
            // 0: log_time, 1: user_name, 2: database_name, 3: process_id,
            // 4: connection_from, 5: session_id, 6: session_line_num,
            // 7: command_tag, 8: session_start_time, 9: virtual_transaction_id,
            // 10: transaction_id, 11: error_severity, 12: sql_state_code,
            // 13: message, ...

            let severity = record.get(11).unwrap_or("");
            if severity != "LOG" {
                continue;
            }

            let message = match record.get(13) {
                Some(msg) => msg,
                None => continue,
            };

            // Parse "duration: X.XXX ms  statement: SQL..."
            // or   "duration: X.XXX ms  execute <name>: SQL..."
            let (duration_us, mut sql) = match parse_duration_statement(message) {
                Some(parsed) => parsed,
                None => continue,
            };

            // For prepared statements, inline parameter values from the detail field
            // Detail field (14) contains: "Parameters: $1 = 'value', $2 = 42, ..."
            if sql.contains('$') {
                if let Some(detail) = record.get(14) {
                    if let Some(params) = parse_parameters(detail) {
                        sql = inline_parameters(&sql, &params);
                    }
                }
            }

            let log_time = record
                .get(0)
                .unwrap_or("")
                .parse::<DateTime<Utc>>()
                .or_else(|_| {
                    let ts = record.get(0).unwrap_or("");
                    let ts = ts.trim();
                    chrono::NaiveDateTime::parse_from_str(
                        ts.trim_end_matches(" UTC"),
                        "%Y-%m-%d %H:%M:%S%.f",
                    )
                    .map(|ndt| ndt.and_utc())
                })
                .unwrap_or_else(|_| Utc::now());

            entries.push(LogEntry {
                log_time,
                user_name: record.get(1).unwrap_or("").to_string(),
                database_name: record.get(2).unwrap_or("").to_string(),
                session_id: record.get(5).unwrap_or("").to_string(),
                duration_us,
                sql,
            });
        }

        Ok(entries)
    }

    fn build_profile(
        &self,
        entries: Vec<LogEntry>,
        source_host: &str,
        pg_version: &str,
    ) -> Result<WorkloadProfile> {
        let mut session_map: HashMap<String, Vec<LogEntry>> = HashMap::new();
        for entry in entries {
            session_map
                .entry(entry.session_id.clone())
                .or_default()
                .push(entry);
        }

        let mut sessions = Vec::new();
        let mut total_queries: u64 = 0;
        let mut session_counter: u64 = 0;
        let mut next_txn_id: u64 = 1;
        let mut global_min_time: Option<DateTime<Utc>> = None;
        let mut global_max_time: Option<DateTime<Utc>> = None;

        for (_session_id, mut entries) in session_map {
            if entries.is_empty() {
                continue;
            }

            entries.sort_by_key(|e| e.log_time);

            let first_time = entries[0].log_time;
            let user = entries[0].user_name.clone();
            let database = entries[0].database_name.clone();

            for e in &entries {
                match global_min_time {
                    None => global_min_time = Some(e.log_time),
                    Some(t) if e.log_time < t => global_min_time = Some(e.log_time),
                    _ => {}
                }
                match global_max_time {
                    None => global_max_time = Some(e.log_time),
                    Some(t) if e.log_time > t => global_max_time = Some(e.log_time),
                    _ => {}
                }
            }

            let mut queries: Vec<Query> = entries
                .iter()
                .map(|e| {
                    let offset = (e.log_time - first_time).num_microseconds().unwrap_or(0) as u64;
                    Query {
                        sql: e.sql.clone(),
                        start_offset_us: offset,
                        duration_us: e.duration_us,
                        kind: QueryKind::from_sql(&e.sql),
                        transaction_id: None,
                        response_values: None,
                    }
                })
                .collect();

            // Assign transaction IDs to queries within BEGIN/COMMIT|ROLLBACK blocks
            assign_transaction_ids(&mut queries, &mut next_txn_id);

            total_queries += queries.len() as u64;
            session_counter += 1;

            sessions.push(Session {
                id: session_counter,
                user,
                database,
                queries,
            });
        }

        sessions.sort_by_key(|s| s.queries.first().map(|q| q.start_offset_us).unwrap_or(0));

        let capture_duration_us = match (global_min_time, global_max_time) {
            (Some(min), Some(max)) => (max - min).num_microseconds().unwrap_or(0) as u64,
            _ => 0,
        };

        Ok(WorkloadProfile {
            version: 2,
            captured_at: Utc::now(),
            source_host: source_host.to_string(),
            pg_version: pg_version.to_string(),
            capture_method: "csv_log".to_string(),
            sessions,
            metadata: Metadata {
                total_queries,
                total_sessions: session_counter,
                capture_duration_us,
                sequence_snapshot: None,
                pk_map: None,
            },
        })
    }
}

fn assign_transaction_ids(queries: &mut [Query], next_txn_id: &mut u64) {
    crate::profile::assign_transaction_ids(queries, next_txn_id);
}

/// Parse PG log message format:
/// - "duration: X.XXX ms  statement: SQL..."
/// - "duration: X.XXX ms  bind <name>: SQL..."     (prepared statements)
/// - "duration: X.XXX ms  execute <name>: SQL..."   (prepared statements)
/// - "duration: X.XXX ms  parse <name>: SQL..."     (prepared statements)
///
/// For bind/execute/parse, the SQL follows "<name>: " after the keyword.
fn parse_duration_statement(message: &str) -> Option<(u64, String)> {
    let message = message.trim();

    if !message.starts_with("duration:") {
        return None;
    }

    // Parse duration value first (common to all formats)
    let dur_start = "duration: ".len();
    let ms_pos = message.find(" ms")?;
    let dur_str = &message[dur_start..ms_pos];
    let dur_ms: f64 = dur_str.trim().parse().ok()?;
    let dur_us = (dur_ms * 1000.0).round() as u64;

    // Find the SQL portion after "  " (double space) following "ms"
    let after_ms = &message[ms_pos + " ms".len()..];
    let content = after_ms.trim_start();

    // Try each known prefix: statement, execute, bind, parse
    // We only capture "statement" (simple query) and "execute" (prepared statement execution).
    // We skip "bind" and "parse" to avoid duplicating prepared-statement queries — the
    // execute entry carries the actual execution duration which is what we want for replay.
    let sql = if let Some(rest) = content.strip_prefix("statement: ") {
        rest.to_string()
    } else if let Some(rest) = strip_named_prefix(content, "execute") {
        rest
    } else {
        return None;
    };

    if sql.is_empty() {
        return None;
    }

    Some((dur_us, sql))
}

/// Strip a named prepared-statement prefix like "execute <name>: SQL" or "bind <name>: SQL".
/// Returns the SQL portion after "<name>: ", or None if the format doesn't match.
fn strip_named_prefix(content: &str, keyword: &str) -> Option<String> {
    let rest = content.strip_prefix(keyword)?.trim_start();
    // The format is "<name>: SQL..." — find the first ": " after the name
    let colon_pos = rest.find(": ")?;
    let sql = &rest[colon_pos + ": ".len()..];
    Some(sql.to_string())
}

/// Parse PG detail field: "Parameters: $1 = 'value', $2 = 42, $3 = NULL"
/// Returns a vec of (param_number, value_string) pairs.
fn parse_parameters(detail: &str) -> Option<Vec<(usize, String)>> {
    let detail = detail.trim();
    let rest = detail.strip_prefix("Parameters:")?;
    let rest = rest.trim();
    if rest.is_empty() {
        return Some(Vec::new());
    }

    let mut params = Vec::new();
    // Split on ", $" but we need to be careful with quoted strings containing commas.
    // Strategy: walk character by character to handle quoting.
    let mut entries = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let chars: Vec<char> = rest.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\'' && !in_quote {
            in_quote = true;
            current.push(chars[i]);
        } else if chars[i] == '\'' && in_quote {
            // Check for escaped quote ''
            if i + 1 < chars.len() && chars[i + 1] == '\'' {
                current.push('\'');
                current.push('\'');
                i += 2;
                continue;
            }
            in_quote = false;
            current.push(chars[i]);
        } else if chars[i] == ',' && !in_quote {
            entries.push(current.trim().to_string());
            current = String::new();
        } else {
            current.push(chars[i]);
        }
        i += 1;
    }
    if !current.trim().is_empty() {
        entries.push(current.trim().to_string());
    }

    for entry in &entries {
        // Each entry: "$1 = 'value'" or "$2 = 42" or "$3 = NULL"
        let entry = entry.trim();
        let entry = entry.strip_prefix('$').unwrap_or(entry);
        let eq_pos = match entry.find(" = ") {
            Some(p) => p,
            None => continue,
        };
        let num: usize = match entry[..eq_pos].trim().parse() {
            Ok(n) => n,
            Err(_) => continue,
        };
        let value = entry[eq_pos + " = ".len()..].trim().to_string();
        params.push((num, value));
    }

    Some(params)
}

/// Replace $1, $2, ... placeholders in SQL with actual parameter values.
fn inline_parameters(sql: &str, params: &[(usize, String)]) -> String {
    let mut result = sql.to_string();
    // Replace in reverse order ($10 before $1) to avoid partial matches
    let mut sorted_params: Vec<&(usize, String)> = params.iter().collect();
    sorted_params.sort_by(|a, b| b.0.cmp(&a.0));

    for (num, value) in sorted_params {
        let placeholder = format!("${num}");
        result = result.replace(&placeholder, value);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration_statement() {
        let (dur, sql) =
            parse_duration_statement("duration: 1.234 ms  statement: SELECT * FROM users").unwrap();
        assert_eq!(dur, 1234);
        assert_eq!(sql, "SELECT * FROM users");
    }

    #[test]
    fn test_parse_duration_statement_sub_ms() {
        let (dur, sql) =
            parse_duration_statement("duration: 0.045 ms  statement: SELECT 1").unwrap();
        assert_eq!(dur, 45);
        assert_eq!(sql, "SELECT 1");
    }

    #[test]
    fn test_parse_duration_statement_rejects_non_duration() {
        assert!(parse_duration_statement("connection authorized: user=app").is_none());
        assert!(parse_duration_statement("").is_none());
    }

    #[test]
    fn test_parse_duration_execute_prepared() {
        let msg = "duration: 9.281 ms  execute stmtcache_15f213b: SELECT u.id, u.name FROM users u WHERE u.email = $1";
        let (dur, sql) = parse_duration_statement(msg).unwrap();
        assert_eq!(dur, 9281);
        assert_eq!(sql, "SELECT u.id, u.name FROM users u WHERE u.email = $1");
    }

    #[test]
    fn test_parse_duration_bind_skipped() {
        // bind entries are skipped to avoid duplicating prepared-statement queries
        let msg = "duration: 2.072 ms  bind stmtcache_abc123: SELECT * FROM orders WHERE id = $1";
        assert!(parse_duration_statement(msg).is_none());
    }

    #[test]
    fn test_parse_duration_parse_skipped() {
        // parse entries are skipped — only execute carries the execution duration
        let msg = "duration: 0.500 ms  parse stmtcache_def456: INSERT INTO logs (msg) VALUES ($1)";
        assert!(parse_duration_statement(msg).is_none());
    }

    #[test]
    fn test_parse_duration_unnamed_prepared() {
        // Unnamed prepared statements use empty string as name
        let msg = "duration: 1.000 ms  execute <unnamed>: SELECT 1";
        let (dur, sql) = parse_duration_statement(msg).unwrap();
        assert_eq!(dur, 1000);
        assert_eq!(sql, "SELECT 1");
    }

    #[test]
    fn test_parse_parameters_basic() {
        let params =
            parse_parameters("Parameters: $1 = 'sales_demo_app', $2 = 42, $3 = NULL").unwrap();
        assert_eq!(params.len(), 3);
        assert_eq!(params[0], (1, "'sales_demo_app'".to_string()));
        assert_eq!(params[1], (2, "42".to_string()));
        assert_eq!(params[2], (3, "NULL".to_string()));
    }

    #[test]
    fn test_parse_parameters_with_embedded_comma() {
        let params = parse_parameters("Parameters: $1 = 'hello, world', $2 = 5").unwrap();
        assert_eq!(params.len(), 2);
        assert_eq!(params[0], (1, "'hello, world'".to_string()));
        assert_eq!(params[1], (2, "5".to_string()));
    }

    #[test]
    fn test_inline_parameters() {
        let sql = "SELECT * FROM users WHERE email = $1 AND id = $2";
        let params = vec![(1, "'alice@corp.com'".to_string()), (2, "42".to_string())];
        let result = inline_parameters(sql, &params);
        assert_eq!(
            result,
            "SELECT * FROM users WHERE email = 'alice@corp.com' AND id = 42"
        );
    }

    #[test]
    fn test_inline_parameters_double_digit() {
        // $10 should not be confused with $1 + "0"
        let sql = "SELECT $1, $10";
        let params = vec![(1, "'a'".to_string()), (10, "'b'".to_string())];
        let result = inline_parameters(sql, &params);
        assert_eq!(result, "SELECT 'a', 'b'");
    }
}
