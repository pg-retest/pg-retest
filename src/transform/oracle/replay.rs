//! Translate a whole captured workload (`.wkl`) to PostgreSQL through the verified-search
//! engine: every statement is multi-pass translated and the chosen translation is the one
//! the oracle accepts; statements no generator can faithfully translate are dropped and
//! reported. The output is a PostgreSQL workload ready for `pg-retest replay`.
//!
//! This is the library core of the `pg-retest oracle-replay` CLI command. The oracle is a
//! parameter: `SyntacticOracle` (no DB, syntactic) for a quick pass, or
//! `GoldenOracle`/`LiveDiffOracle` for a behavioral one.

use std::collections::BTreeMap;

use crate::profile::{Query, QueryKind, Session, SourceDialect, WorkloadProfile};

use super::engine::{translate_verified, CandidateGenerator};
use super::Oracle;

/// What happened when translating a workload.
#[derive(Debug, Default)]
pub struct OracleReplayReport {
    pub total: usize,
    pub translated: usize,
    pub skipped: usize,
    /// Accepted-translation counts per winning generator.
    pub by_generator: BTreeMap<String, usize>,
    /// `(sql preview, reason)` for each dropped statement.
    pub skips: Vec<(String, String)>,
}

/// Translate every statement in `profile` via the verified-search engine. Returns the
/// PostgreSQL workload (skipped statements dropped, `original_sql` retained on the rest,
/// `source_dialect` set to `Postgres`) and a report.
///
/// NOTE: skipped statements are dropped individually; transaction-aware skipping (dropping
/// a whole transaction when any of its statements is unverifiable) is a follow-on.
pub async fn translate_profile(
    profile: &WorkloadProfile,
    generators: &[Box<dyn CandidateGenerator>],
    oracle: &dyn Oracle,
) -> (WorkloadProfile, OracleReplayReport) {
    let mut report = OracleReplayReport::default();
    let mut new_sessions = Vec::with_capacity(profile.sessions.len());

    for session in &profile.sessions {
        let mut new_queries = Vec::with_capacity(session.queries.len());
        for q in &session.queries {
            report.total += 1;
            // Reference = the original MySQL statement (used by behavioral oracles;
            // ignored by SyntacticOracle).
            let r = translate_verified(generators, oracle, &q.sql, &q.sql).await;
            match (r.winner, r.accepted_sql) {
                (Some(winner), Some(translated_sql)) => {
                    report.translated += 1;
                    *report.by_generator.entry(winner.to_string()).or_default() += 1;
                    new_queries.push(Query {
                        kind: QueryKind::from_sql(&translated_sql),
                        sql: translated_sql,
                        start_offset_us: q.start_offset_us,
                        duration_us: q.duration_us,
                        transaction_id: q.transaction_id,
                        response_values: q.response_values.clone(),
                        original_sql: Some(q.sql.clone()), // retain the source-native SQL
                    });
                }
                _ => {
                    report.skipped += 1;
                    let preview: String = q.sql.chars().take(60).collect();
                    report.skips.push((
                        preview,
                        "no generator produced a verified PostgreSQL translation".to_string(),
                    ));
                }
            }
        }
        new_sessions.push(Session {
            id: session.id,
            user: session.user.clone(),
            database: session.database.clone(),
            queries: new_queries,
        });
    }

    let total_queries = new_sessions.iter().map(|s| s.queries.len()).sum::<usize>() as u64;
    let mut metadata = profile.metadata.clone();
    metadata.total_queries = total_queries;

    let translated = WorkloadProfile {
        version: profile.version,
        captured_at: profile.captured_at,
        source_host: profile.source_host.clone(),
        pg_version: profile.pg_version.clone(),
        capture_method: format!("{}+oracle-translated", profile.capture_method),
        sessions: new_sessions,
        metadata,
        source_dialect: SourceDialect::Postgres, // the workload is now PostgreSQL
    };
    (translated, report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Metadata;
    use crate::transform::mysql_to_pg::mysql_to_pg_pipeline;
    use crate::transform::oracle::engine::PipelineGenerator;
    use crate::transform::oracle::SyntacticOracle;
    use crate::transform::polyglot::mysql_to_pg_polyglot_pipeline;

    fn q(sql: &str) -> Query {
        Query {
            sql: sql.into(),
            start_offset_us: 0,
            duration_us: 10,
            kind: QueryKind::from_sql(sql),
            transaction_id: None,
            response_values: None,
            original_sql: None,
        }
    }

    fn mysql_profile() -> WorkloadProfile {
        WorkloadProfile {
            version: 2,
            captured_at: chrono::Utc::now(),
            source_host: "mysql".into(),
            pg_version: "8.0".into(),
            capture_method: "mysql_slow_log".into(),
            source_dialect: SourceDialect::MySql,
            sessions: vec![Session {
                id: 1,
                user: "app".into(),
                database: "db".into(),
                queries: vec![
                    q("SELECT IFNULL(name, 'x') FROM t"),
                    q("SELECT `id` FROM `t`"),
                    // ON DUPLICATE KEY UPDATE is invalid PG syntax (PG's parser rejects
                    // it) and no generator translates it → skipped by all.
                    q("INSERT INTO t (id) VALUES (1) ON DUPLICATE KEY UPDATE id = id"),
                ],
            }],
            metadata: Metadata {
                total_queries: 3,
                total_sessions: 1,
                capture_duration_us: 0,
                sequence_snapshot: None,
                pk_map: None,
            },
        }
    }

    #[tokio::test]
    async fn test_translate_profile_syntactic() {
        let profile = mysql_profile();
        let generators: Vec<Box<dyn CandidateGenerator>> = vec![
            PipelineGenerator::boxed("regex", mysql_to_pg_pipeline()),
            PipelineGenerator::boxed("polyglot", mysql_to_pg_polyglot_pipeline()),
        ];
        let (out, report) = translate_profile(&profile, &generators, &SyntacticOracle).await;

        assert_eq!(report.total, 3);
        assert_eq!(report.translated, 2); // IFNULL + backticks
        assert_eq!(report.skipped, 1); // ON DUPLICATE KEY (invalid PG, no generator translates)
        assert_eq!(out.source_dialect, SourceDialect::Postgres);
        assert_eq!(out.sessions[0].queries.len(), 2);
        // original MySQL retained; translated to valid PG.
        let first = &out.sessions[0].queries[0];
        assert!(first.sql.contains("COALESCE"));
        assert_eq!(
            first.original_sql.as_deref(),
            Some("SELECT IFNULL(name, 'x') FROM t")
        );
        assert!(crate::transform::is_valid_postgres(
            &out.sessions[0].queries[1].sql
        ));
        assert_eq!(out.metadata.total_queries, 2);
    }
}
