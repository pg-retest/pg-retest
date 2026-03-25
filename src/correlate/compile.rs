use crate::profile::WorkloadProfile;
use anyhow::{bail, Result};
use std::collections::HashSet;
use tracing::info;

#[derive(Debug)]
pub struct CompileStats {
    pub queries_with_responses: usize,
    pub unique_captured_ids: usize,
    pub queries_referencing_ids: usize,
    pub total_id_references: usize,
}

/// Compile a workload by stripping response_values and validating ID consistency.
/// The compiled workload replays without --id-mode — IDs are pre-resolved for PITR + seq reset.
pub fn compile_workload(mut profile: WorkloadProfile) -> Result<(WorkloadProfile, CompileStats)> {
    // 1. Collect all captured response values (the "known IDs")
    let mut known_ids: HashSet<String> = HashSet::new();
    let mut queries_with_responses = 0;

    for session in &profile.sessions {
        for query in &session.queries {
            if let Some(ref rvs) = query.response_values {
                queries_with_responses += 1;
                for rv in rvs {
                    for (_, val) in &rv.columns {
                        known_ids.insert(val.clone());
                    }
                }
            }
        }
    }

    if queries_with_responses == 0 {
        bail!(
            "No response_values found in workload. \
             Capture with --id-mode=correlate or --id-mode=full to enable compilation."
        );
    }

    // 2. Audit: scan all queries for references to known IDs
    let mut queries_referencing_ids = 0;
    let mut total_id_references = 0;

    for session in &profile.sessions {
        for query in &session.queries {
            let mut refs_in_query = 0;
            // Simple scan: check if any known ID appears as a word boundary in the SQL
            for id in &known_ids {
                let sql = &query.sql;
                let id_bytes = id.as_bytes();
                let sql_bytes = sql.as_bytes();
                let mut pos = 0;
                while pos < sql.len() {
                    if let Some(found) = sql[pos..].find(id.as_str()) {
                        let abs_pos = pos + found;
                        // Check word boundaries
                        let before_ok = abs_pos == 0
                            || (!sql_bytes[abs_pos - 1].is_ascii_alphanumeric()
                                && sql_bytes[abs_pos - 1] != b'_');
                        let after_pos = abs_pos + id_bytes.len();
                        let after_ok = after_pos >= sql_bytes.len()
                            || (!sql_bytes[after_pos].is_ascii_alphanumeric()
                                && sql_bytes[after_pos] != b'_');
                        if before_ok && after_ok {
                            refs_in_query += 1;
                        }
                        pos = abs_pos + 1;
                    } else {
                        break;
                    }
                }
            }
            if refs_in_query > 0 {
                queries_referencing_ids += 1;
                total_id_references += refs_in_query;
            }
        }
    }

    // 3. Strip response_values from all queries
    for session in &mut profile.sessions {
        for query in &mut session.queries {
            query.response_values = None;
        }
    }

    // 4. Update metadata
    if !profile.capture_method.contains("+compiled") {
        profile.capture_method = format!("{}+compiled", profile.capture_method);
    }

    let stats = CompileStats {
        queries_with_responses,
        unique_captured_ids: known_ids.len(),
        queries_referencing_ids,
        total_id_references,
    };

    info!(
        "Compilation complete: {} queries with responses, {} unique IDs, {} referencing queries, {} total references",
        stats.queries_with_responses, stats.unique_captured_ids,
        stats.queries_referencing_ids, stats.total_id_references
    );

    Ok((profile, stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::correlate::capture::ResponseRow;
    use crate::profile::*;
    use chrono::Utc;

    fn make_profile(queries: Vec<Query>) -> WorkloadProfile {
        WorkloadProfile {
            version: 2,
            captured_at: Utc::now(),
            source_host: "localhost".into(),
            pg_version: "16.2".into(),
            capture_method: "proxy".into(),
            sessions: vec![Session {
                id: 1,
                user: "app".into(),
                database: "db".into(),
                queries,
            }],
            metadata: Metadata {
                total_queries: 1,
                total_sessions: 1,
                capture_duration_us: 100,
                sequence_snapshot: Some(vec![]),
                pk_map: None,
            },
        }
    }

    #[test]
    fn test_compile_strips_response_values() {
        let profile = make_profile(vec![
            Query {
                sql: "INSERT INTO t (x) VALUES (1) RETURNING id".into(),
                start_offset_us: 0,
                duration_us: 100,
                kind: QueryKind::Insert,
                transaction_id: None,
                response_values: Some(vec![ResponseRow {
                    columns: vec![("id".into(), "42".into())],
                }]),
            },
            Query {
                sql: "SELECT * FROM t WHERE id = 42".into(),
                start_offset_us: 100,
                duration_us: 50,
                kind: QueryKind::Select,
                transaction_id: None,
                response_values: None,
            },
        ]);
        let (compiled, stats) = compile_workload(profile).unwrap();
        assert!(compiled.sessions[0].queries[0].response_values.is_none());
        assert!(compiled.sessions[0].queries[1].response_values.is_none());
        assert_eq!(stats.queries_with_responses, 1);
        assert_eq!(stats.unique_captured_ids, 1);
    }

    #[test]
    fn test_compile_preserves_sequence_snapshot() {
        let profile = make_profile(vec![Query {
            sql: "INSERT INTO t (x) VALUES (1) RETURNING id".into(),
            start_offset_us: 0,
            duration_us: 100,
            kind: QueryKind::Insert,
            transaction_id: None,
            response_values: Some(vec![ResponseRow {
                columns: vec![("id".into(), "1".into())],
            }]),
        }]);
        let (compiled, _) = compile_workload(profile).unwrap();
        assert!(compiled.metadata.sequence_snapshot.is_some());
    }

    #[test]
    fn test_compile_updates_capture_method() {
        let profile = make_profile(vec![Query {
            sql: "INSERT INTO t (x) VALUES (1) RETURNING id".into(),
            start_offset_us: 0,
            duration_us: 100,
            kind: QueryKind::Insert,
            transaction_id: None,
            response_values: Some(vec![ResponseRow {
                columns: vec![("id".into(), "1".into())],
            }]),
        }]);
        let (compiled, _) = compile_workload(profile).unwrap();
        assert_eq!(compiled.capture_method, "proxy+compiled");
    }

    #[test]
    fn test_compile_no_response_values_error() {
        let profile = make_profile(vec![Query {
            sql: "SELECT 1".into(),
            start_offset_us: 0,
            duration_us: 100,
            kind: QueryKind::Select,
            transaction_id: None,
            response_values: None,
        }]);
        assert!(compile_workload(profile).is_err());
    }

    #[test]
    fn test_compile_finds_references() {
        let profile = make_profile(vec![
            Query {
                sql: "INSERT INTO orders (cid) VALUES (1) RETURNING id".into(),
                start_offset_us: 0,
                duration_us: 100,
                kind: QueryKind::Insert,
                transaction_id: None,
                response_values: Some(vec![ResponseRow {
                    columns: vec![("id".into(), "42".into())],
                }]),
            },
            Query {
                sql: "INSERT INTO items (order_id) VALUES (42)".into(),
                start_offset_us: 100,
                duration_us: 50,
                kind: QueryKind::Insert,
                transaction_id: None,
                response_values: None,
            },
            Query {
                sql: "SELECT * FROM orders WHERE id = 42".into(),
                start_offset_us: 200,
                duration_us: 30,
                kind: QueryKind::Select,
                transaction_id: None,
                response_values: None,
            },
        ]);
        let (_, stats) = compile_workload(profile).unwrap();
        assert_eq!(stats.queries_referencing_ids, 2); // items INSERT + SELECT
        assert_eq!(stats.total_id_references, 2);
    }

    #[test]
    fn test_compile_idempotent_capture_method() {
        let mut profile = make_profile(vec![Query {
            sql: "INSERT INTO t (x) VALUES (1) RETURNING id".into(),
            start_offset_us: 0,
            duration_us: 100,
            kind: QueryKind::Insert,
            transaction_id: None,
            response_values: Some(vec![ResponseRow {
                columns: vec![("id".into(), "1".into())],
            }]),
        }]);
        // Pre-set the capture method to already include +compiled
        profile.capture_method = "proxy+compiled".into();
        let (compiled, _) = compile_workload(profile).unwrap();
        // Should not double-append
        assert_eq!(compiled.capture_method, "proxy+compiled");
    }

    #[test]
    fn test_compile_preserves_sql_unchanged() {
        let original_sql = "INSERT INTO t (x) VALUES (1) RETURNING id";
        let select_sql = "SELECT * FROM t WHERE id = 99";
        let profile = make_profile(vec![
            Query {
                sql: original_sql.into(),
                start_offset_us: 0,
                duration_us: 100,
                kind: QueryKind::Insert,
                transaction_id: None,
                response_values: Some(vec![ResponseRow {
                    columns: vec![("id".into(), "99".into())],
                }]),
            },
            Query {
                sql: select_sql.into(),
                start_offset_us: 100,
                duration_us: 50,
                kind: QueryKind::Select,
                transaction_id: None,
                response_values: None,
            },
        ]);
        let (compiled, _) = compile_workload(profile).unwrap();
        assert_eq!(compiled.sessions[0].queries[0].sql, original_sql);
        assert_eq!(compiled.sessions[0].queries[1].sql, select_sql);
    }
}
