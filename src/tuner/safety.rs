use super::types::Recommendation;
use anyhow::{bail, Result};

/// PostgreSQL configuration parameters that are safe to change via ALTER SYSTEM.
/// These are performance-tuning knobs that do not affect data integrity,
/// security, or connectivity.
pub const SAFE_CONFIG_PARAMS: &[&str] = &[
    "shared_buffers",
    "work_mem",
    "maintenance_work_mem",
    "effective_cache_size",
    "temp_buffers",
    "huge_pages",
    "random_page_cost",
    "seq_page_cost",
    "cpu_tuple_cost",
    "cpu_index_tuple_cost",
    "cpu_operator_cost",
    "default_statistics_target",
    "enable_seqscan",
    "enable_indexscan",
    "enable_bitmapscan",
    "enable_hashjoin",
    "enable_mergejoin",
    "enable_nestloop",
    "enable_hashagg",
    "enable_material",
    "enable_sort",
    "max_parallel_workers_per_gather",
    "max_parallel_workers",
    "max_parallel_maintenance_workers",
    "parallel_tuple_cost",
    "parallel_setup_cost",
    "min_parallel_table_scan_size",
    "min_parallel_index_scan_size",
    "checkpoint_completion_target",
    "wal_buffers",
    "commit_delay",
    "commit_siblings",
    "jit",
    "jit_above_cost",
    "jit_inline_above_cost",
    "jit_optimize_above_cost",
    "autovacuum_vacuum_scale_factor",
    "autovacuum_analyze_scale_factor",
    "autovacuum_vacuum_cost_delay",
    "autovacuum_vacuum_cost_limit",
    "log_min_duration_statement",
];

/// Configuration parameters that must never be changed by the tuner.
/// These affect connectivity, security, and data directory layout.
pub const BLOCKED_CONFIG_PARAMS: &[&str] = &[
    "data_directory",
    "listen_addresses",
    "port",
    "hba_file",
    "pg_hba_file",
    "ident_file",
    "ssl_cert_file",
    "ssl_key_file",
    "ssl_ca_file",
    "password_encryption",
    "log_directory",
];

/// Hostname patterns that suggest a production environment.
const PRODUCTION_PATTERNS: &[&str] = &["prod", "production", "primary", "master", "main"];

/// Check whether the target connection string contains a production hostname pattern.
/// If it does and `force` is false, return an error to prevent accidental tuning
/// of production databases.
pub fn check_production_hostname(target: &str, force: bool) -> Result<()> {
    let lower = target.to_lowercase();
    for pattern in PRODUCTION_PATTERNS {
        if lower.contains(pattern) {
            if force {
                return Ok(());
            }
            bail!(
                "Target '{}' looks like a production server (matched pattern '{}').\n\
                 Use --force to override this safety check.",
                target,
                pattern
            );
        }
    }
    Ok(())
}

/// Check whether a PG parameter name is on the safe allowlist.
/// Comparison is case-insensitive.
pub fn is_safe_param(param: &str) -> bool {
    let lower = param.to_lowercase();
    SAFE_CONFIG_PARAMS.iter().any(|&safe| safe == lower)
}

/// Check whether a SQL statement is blocked from execution.
/// Returns `Some(reason)` if the statement is dangerous, `None` if it is safe.
fn is_blocked_sql(sql: &str) -> Option<String> {
    let upper = sql.trim().to_uppercase();

    if upper.starts_with("DROP TABLE") {
        return Some("DROP TABLE is not allowed".into());
    }
    if upper.starts_with("DROP DATABASE") {
        return Some("DROP DATABASE is not allowed".into());
    }
    if upper.starts_with("DROP SCHEMA") {
        return Some("DROP SCHEMA is not allowed".into());
    }
    if upper.starts_with("TRUNCATE") {
        return Some("TRUNCATE is not allowed".into());
    }
    // ALTER TABLE ... DROP COLUMN
    if upper.starts_with("ALTER TABLE") && upper.contains("DROP COLUMN") {
        return Some("ALTER TABLE ... DROP COLUMN is not allowed".into());
    }

    None
}

/// Validate a list of recommendations, splitting them into safe and rejected.
/// Returns `(safe, rejected_with_reasons)`.
pub fn validate_recommendations(
    recs: &[Recommendation],
) -> (Vec<Recommendation>, Vec<(Recommendation, String)>) {
    let mut safe = Vec::new();
    let mut rejected: Vec<(Recommendation, String)> = Vec::new();

    for rec in recs {
        match rec {
            Recommendation::ConfigChange { parameter, .. } => {
                if is_safe_param(parameter) {
                    safe.push(rec.clone());
                } else {
                    rejected.push((
                        rec.clone(),
                        format!("Parameter '{}' is not on the safe allowlist", parameter),
                    ));
                }
            }
            Recommendation::CreateIndex { sql, .. } => {
                if let Some(reason) = is_blocked_sql(sql) {
                    rejected.push((rec.clone(), reason));
                } else {
                    safe.push(rec.clone());
                }
            }
            Recommendation::QueryRewrite { .. } => {
                // Query rewrites are always safe (informational only)
                safe.push(rec.clone());
            }
            Recommendation::SchemaChange { sql, .. } => {
                if let Some(reason) = is_blocked_sql(sql) {
                    rejected.push((rec.clone(), reason));
                } else {
                    safe.push(rec.clone());
                }
            }
        }
    }

    (safe, rejected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_params() {
        // Known safe param
        assert!(is_safe_param("shared_buffers"));
        // Blocked param should not be safe
        assert!(!is_safe_param("data_directory"));
        // Unknown param should not be safe
        assert!(!is_safe_param("some_unknown_param"));
        // Case insensitive
        assert!(is_safe_param("Shared_Buffers"));
        assert!(is_safe_param("WORK_MEM"));
    }

    #[test]
    fn test_production_hostname_check() {
        // Production hostname should be blocked
        assert!(check_production_hostname("postgresql://prod-db:5432/mydb", false).is_err());
        assert!(
            check_production_hostname("postgresql://production.example.com/mydb", false).is_err()
        );
        // Localhost should be fine
        assert!(check_production_hostname("postgresql://localhost/mydb", false).is_ok());
        // Force overrides the check
        assert!(check_production_hostname("postgresql://prod-db:5432/mydb", true).is_ok());
    }

    #[test]
    fn test_validate_recommendations() {
        let recs = vec![
            // Safe config change
            Recommendation::ConfigChange {
                parameter: "shared_buffers".into(),
                current_value: "128MB".into(),
                recommended_value: "1GB".into(),
                rationale: "More memory".into(),
            },
            // Unsafe config change (blocked param)
            Recommendation::ConfigChange {
                parameter: "data_directory".into(),
                current_value: "/var/lib/pg".into(),
                recommended_value: "/mnt/fast/pg".into(),
                rationale: "Faster disk".into(),
            },
            // Safe index creation
            Recommendation::CreateIndex {
                table: "orders".into(),
                columns: vec!["status".into()],
                index_type: None,
                sql: "CREATE INDEX idx_orders_status ON orders (status)".into(),
                rationale: "Frequent filter".into(),
            },
            // Unsafe schema change (DROP TABLE)
            Recommendation::SchemaChange {
                sql: "DROP TABLE old_data".into(),
                description: "Remove old table".into(),
                rationale: "Cleanup".into(),
            },
            // Safe query rewrite (always safe)
            Recommendation::QueryRewrite {
                original_sql: "SELECT *".into(),
                rewritten_sql: "SELECT id".into(),
                rationale: "Narrow select".into(),
            },
        ];

        let (safe, rejected) = validate_recommendations(&recs);
        assert_eq!(safe.len(), 3);
        assert_eq!(rejected.len(), 2);

        // Verify reasons
        assert!(rejected[0].1.contains("not on the safe allowlist"));
        assert!(rejected[1].1.contains("DROP TABLE"));
    }

    #[test]
    fn test_blocked_sql() {
        // Should be blocked
        assert!(is_blocked_sql("DROP TABLE users").is_some());
        assert!(is_blocked_sql("TRUNCATE orders").is_some());
        assert!(is_blocked_sql("ALTER TABLE users DROP COLUMN email").is_some());
        assert!(is_blocked_sql("DROP DATABASE mydb").is_some());
        assert!(is_blocked_sql("DROP SCHEMA public").is_some());

        // Should be allowed
        assert!(is_blocked_sql("CREATE INDEX idx_foo ON bar (baz)").is_none());
        assert!(is_blocked_sql("ALTER TABLE users ADD COLUMN archived boolean").is_none());
        assert!(is_blocked_sql("SELECT 1").is_none());
    }
}
