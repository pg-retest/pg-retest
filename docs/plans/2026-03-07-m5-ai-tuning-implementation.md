# M5: AI-Assisted Tuning Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Build an AI-powered database tuning system that collects PG context, generates recommendations via LLM, applies changes, replays workloads, and iterates until performance improves.

**Architecture:** Monolithic TuningOrchestrator owns a configurable loop: collect PG context → call LLM for recommendations → validate safety → apply changes → replay workload → compare results → feed back to LLM → repeat. Multi-provider LLM support (Claude/OpenAI/Ollama) reuses existing transform infrastructure. Safety layer with parameter allowlist and dry-run default.

**Tech Stack:** Rust, tokio-postgres (PG introspection), reqwest (LLM HTTP), serde/serde_json (serialization), existing replay/compare/analyze infrastructure.

---

### Task 1: Tuning Data Types

**Files:**
- Create: `src/tuner/mod.rs`
- Create: `src/tuner/types.rs`
- Modify: `src/lib.rs` (add `pub mod tuner;`)

**Step 1: Create `src/tuner/types.rs` with all data types**

```rust
use serde::{Deserialize, Serialize};

/// A single tuning recommendation from the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Recommendation {
    ConfigChange {
        parameter: String,
        current_value: String,
        recommended_value: String,
        rationale: String,
    },
    CreateIndex {
        table: String,
        columns: Vec<String>,
        index_type: Option<String>,
        sql: String,
        rationale: String,
    },
    QueryRewrite {
        original_sql: String,
        rewritten_sql: String,
        rationale: String,
    },
    SchemaChange {
        sql: String,
        description: String,
        rationale: String,
    },
}

/// Tracks whether a recommendation was successfully applied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppliedChange {
    pub recommendation: Recommendation,
    pub success: bool,
    pub error: Option<String>,
    pub rollback_sql: Option<String>,
}

/// Summary of comparison metrics for one iteration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComparisonSummary {
    pub p50_change_pct: f64,
    pub p95_change_pct: f64,
    pub p99_change_pct: f64,
    pub regressions: usize,
    pub improvements: usize,
    pub errors_delta: i64,
}

/// Result of one tuning iteration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuningIteration {
    pub iteration: u32,
    pub recommendations: Vec<Recommendation>,
    pub applied: Vec<AppliedChange>,
    pub comparison: Option<ComparisonSummary>,
    pub llm_feedback: String,
}

/// Full tuning session result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TuningReport {
    pub workload: String,
    pub target: String,
    pub provider: String,
    pub hint: Option<String>,
    pub iterations: Vec<TuningIteration>,
    pub total_improvement_pct: f64,
    pub all_changes: Vec<AppliedChange>,
}

/// Configuration for a tuning session.
#[derive(Debug, Clone)]
pub struct TuningConfig {
    pub workload_path: std::path::PathBuf,
    pub target: String,
    pub provider: String,
    pub api_key: Option<String>,
    pub api_url: Option<String>,
    pub model: Option<String>,
    pub max_iterations: u32,
    pub hint: Option<String>,
    pub apply: bool,
    pub force: bool,
    pub speed: f64,
    pub read_only: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recommendation_json_roundtrip() {
        let recs = vec![
            Recommendation::ConfigChange {
                parameter: "shared_buffers".into(),
                current_value: "128MB".into(),
                recommended_value: "1GB".into(),
                rationale: "More memory".into(),
            },
            Recommendation::CreateIndex {
                table: "orders".into(),
                columns: vec!["status".into(), "created_at".into()],
                index_type: Some("btree".into()),
                sql: "CREATE INDEX idx_orders_status ON orders (status, created_at)".into(),
                rationale: "Frequent filter".into(),
            },
            Recommendation::QueryRewrite {
                original_sql: "SELECT * FROM orders".into(),
                rewritten_sql: "SELECT id, status FROM orders".into(),
                rationale: "Select only needed columns".into(),
            },
            Recommendation::SchemaChange {
                sql: "ALTER TABLE orders ADD COLUMN archived boolean DEFAULT false".into(),
                description: "Add archive flag".into(),
                rationale: "Partition active/archived".into(),
            },
        ];
        let json = serde_json::to_string(&recs).unwrap();
        let parsed: Vec<Recommendation> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 4);
    }

    #[test]
    fn test_tuning_report_serialization() {
        let report = TuningReport {
            workload: "test.wkl".into(),
            target: "postgresql://localhost/test".into(),
            provider: "claude".into(),
            hint: Some("focus on reads".into()),
            iterations: vec![],
            total_improvement_pct: 0.0,
            all_changes: vec![],
        };
        let json = serde_json::to_string_pretty(&report).unwrap();
        let parsed: TuningReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.workload, "test.wkl");
        assert_eq!(parsed.hint, Some("focus on reads".into()));
    }
}
```

**Step 2: Create `src/tuner/mod.rs`**

```rust
pub mod types;
```

**Step 3: Add `pub mod tuner;` to `src/lib.rs`**

Add after the existing `pub mod transform;` line.

**Step 4: Run tests**

Run: `cargo test --lib tuner::types`
Expected: 2 tests pass

**Step 5: Commit**

```bash
git add src/tuner/ src/lib.rs
git commit -m "feat(tuner): add tuning data types — Recommendation, TuningReport, TuningConfig"
```

---

### Task 2: Safety Module

**Files:**
- Create: `src/tuner/safety.rs`
- Modify: `src/tuner/mod.rs` (add `pub mod safety;`)

**Step 1: Create `src/tuner/safety.rs`**

```rust
use anyhow::{bail, Result};

use super::types::Recommendation;

/// PostgreSQL configuration parameters that are safe to modify.
const SAFE_CONFIG_PARAMS: &[&str] = &[
    // Memory
    "shared_buffers", "work_mem", "maintenance_work_mem",
    "effective_cache_size", "temp_buffers", "huge_pages",
    // Planner cost
    "random_page_cost", "seq_page_cost", "cpu_tuple_cost",
    "cpu_index_tuple_cost", "cpu_operator_cost",
    "default_statistics_target",
    // Planner enable/disable
    "enable_seqscan", "enable_indexscan", "enable_bitmapscan",
    "enable_hashjoin", "enable_mergejoin", "enable_nestloop",
    "enable_hashagg", "enable_material", "enable_sort",
    // Parallelism
    "max_parallel_workers_per_gather", "max_parallel_workers",
    "max_parallel_maintenance_workers", "parallel_tuple_cost",
    "parallel_setup_cost", "min_parallel_table_scan_size",
    "min_parallel_index_scan_size",
    // WAL & Checkpoint
    "checkpoint_completion_target", "wal_buffers",
    "commit_delay", "commit_siblings",
    // JIT
    "jit", "jit_above_cost", "jit_inline_above_cost",
    "jit_optimize_above_cost",
    // Autovacuum
    "autovacuum_vacuum_scale_factor", "autovacuum_analyze_scale_factor",
    "autovacuum_vacuum_cost_delay", "autovacuum_vacuum_cost_limit",
    // Logging (safe)
    "log_min_duration_statement",
];

/// SQL keywords that indicate a dangerous operation.
const BLOCKED_SQL_PATTERNS: &[&str] = &[
    "DROP DATABASE",
    "DROP SCHEMA",
    "DROP TABLE",
    "TRUNCATE",
    "DELETE FROM pg_",
    "ALTER TABLE", // checked more specifically below
];

/// Dangerous config parameters that must never be changed.
const BLOCKED_CONFIG_PARAMS: &[&str] = &[
    "data_directory", "listen_addresses", "port",
    "hba_file", "pg_hba_file", "ident_file",
    "ssl_cert_file", "ssl_key_file", "ssl_ca_file",
    "password_encryption", "log_directory",
];

/// Production-looking hostname patterns.
const PRODUCTION_PATTERNS: &[&str] = &[
    "prod", "production", "primary", "master", "main",
];

/// Check if a target connection string looks like production.
pub fn check_production_hostname(target: &str, force: bool) -> Result<()> {
    if force {
        return Ok(());
    }
    let lower = target.to_lowercase();
    for pattern in PRODUCTION_PATTERNS {
        if lower.contains(pattern) {
            bail!(
                "Target '{}' looks like a production database (contains '{}'). \
                 Use --force to override this safety check.",
                target, pattern
            );
        }
    }
    Ok(())
}

/// Check if a config parameter is safe to modify.
pub fn is_safe_param(param: &str) -> bool {
    let lower = param.to_lowercase();
    SAFE_CONFIG_PARAMS.iter().any(|s| *s == lower)
}

/// Check if a SQL statement contains blocked operations.
fn is_blocked_sql(sql: &str) -> Option<String> {
    let upper = sql.to_uppercase();

    // Check explicit blocked patterns
    for pattern in BLOCKED_SQL_PATTERNS {
        if *pattern == "ALTER TABLE" {
            // Only block ALTER TABLE ... DROP COLUMN
            if upper.contains("ALTER TABLE") && upper.contains("DROP COLUMN") {
                return Some("ALTER TABLE ... DROP COLUMN is blocked".into());
            }
        } else if upper.contains(pattern) {
            return Some(format!("{} is blocked", pattern));
        }
    }
    None
}

/// Validate a list of recommendations against safety rules.
/// Returns (safe_recs, rejected_recs_with_reasons).
pub fn validate_recommendations(
    recs: &[Recommendation],
) -> (Vec<Recommendation>, Vec<(Recommendation, String)>) {
    let mut safe = Vec::new();
    let mut rejected = Vec::new();

    for rec in recs {
        match rec {
            Recommendation::ConfigChange { parameter, .. } => {
                let lower = parameter.to_lowercase();
                if BLOCKED_CONFIG_PARAMS.iter().any(|b| *b == lower) {
                    rejected.push((
                        rec.clone(),
                        format!("Parameter '{}' is explicitly blocked", parameter),
                    ));
                } else if !is_safe_param(parameter) {
                    rejected.push((
                        rec.clone(),
                        format!("Parameter '{}' is not in the safety allowlist", parameter),
                    ));
                } else {
                    safe.push(rec.clone());
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
                // Query rewrites are informational — they don't modify the database
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
        assert!(is_safe_param("shared_buffers"));
        assert!(is_safe_param("work_mem"));
        assert!(is_safe_param("Shared_Buffers")); // case insensitive
        assert!(!is_safe_param("data_directory"));
        assert!(!is_safe_param("listen_addresses"));
        assert!(!is_safe_param("unknown_param"));
    }

    #[test]
    fn test_production_hostname_check() {
        assert!(check_production_hostname("postgresql://prod-db:5432/app", false).is_err());
        assert!(check_production_hostname("postgresql://production.example.com/db", false).is_err());
        assert!(check_production_hostname("postgresql://primary-db/app", false).is_err());
        assert!(check_production_hostname("postgresql://localhost/testdb", false).is_ok());
        assert!(check_production_hostname("postgresql://staging.example.com/db", false).is_ok());
        // --force overrides
        assert!(check_production_hostname("postgresql://prod-db:5432/app", true).is_ok());
    }

    #[test]
    fn test_validate_recommendations() {
        let recs = vec![
            Recommendation::ConfigChange {
                parameter: "shared_buffers".into(),
                current_value: "128MB".into(),
                recommended_value: "1GB".into(),
                rationale: "".into(),
            },
            Recommendation::ConfigChange {
                parameter: "data_directory".into(),
                current_value: "/data".into(),
                recommended_value: "/new".into(),
                rationale: "".into(),
            },
            Recommendation::ConfigChange {
                parameter: "some_unknown_param".into(),
                current_value: "1".into(),
                recommended_value: "2".into(),
                rationale: "".into(),
            },
            Recommendation::CreateIndex {
                table: "orders".into(),
                columns: vec!["status".into()],
                index_type: None,
                sql: "CREATE INDEX idx_orders_status ON orders (status)".into(),
                rationale: "".into(),
            },
            Recommendation::SchemaChange {
                sql: "DROP TABLE users".into(),
                description: "Remove users".into(),
                rationale: "".into(),
            },
        ];
        let (safe, rejected) = validate_recommendations(&recs);
        assert_eq!(safe.len(), 2); // shared_buffers + CREATE INDEX
        assert_eq!(rejected.len(), 3); // data_directory + unknown + DROP TABLE
    }

    #[test]
    fn test_blocked_sql() {
        assert!(is_blocked_sql("DROP TABLE users").is_some());
        assert!(is_blocked_sql("TRUNCATE orders").is_some());
        assert!(is_blocked_sql("ALTER TABLE users DROP COLUMN email").is_some());
        assert!(is_blocked_sql("ALTER TABLE users ADD COLUMN age int").is_none());
        assert!(is_blocked_sql("CREATE INDEX idx ON orders (status)").is_none());
        assert!(is_blocked_sql("SELECT 1").is_none());
    }
}
```

**Step 2: Add `pub mod safety;` to `src/tuner/mod.rs`**

**Step 3: Run tests**

Run: `cargo test --lib tuner::safety`
Expected: 4 tests pass

**Step 4: Commit**

```bash
git add src/tuner/safety.rs src/tuner/mod.rs
git commit -m "feat(tuner): add safety module with allowlist, blocked ops, production check"
```

---

### Task 3: PG Context Collector

**Files:**
- Create: `src/tuner/context.rs`
- Modify: `src/tuner/mod.rs` (add `pub mod context;`)

**Step 1: Create `src/tuner/context.rs`**

This module connects to a target PG database and collects introspection data.

```rust
use anyhow::Result;
use serde::Serialize;
use tokio_postgres::{Client, NoTls};

use crate::profile::WorkloadProfile;

/// PostgreSQL setting from pg_settings.
#[derive(Debug, Clone, Serialize)]
pub struct PgSetting {
    pub name: String,
    pub setting: String,
    pub unit: Option<String>,
    pub source: String,
}

/// Slow query info from the workload profile.
#[derive(Debug, Clone, Serialize)]
pub struct SlowQuery {
    pub sql: String,
    pub duration_us: u64,
    pub occurrences: usize,
}

/// pg_stat_statements row (if extension is available).
#[derive(Debug, Clone, Serialize)]
pub struct StatStatement {
    pub query: String,
    pub calls: i64,
    pub mean_exec_time_ms: f64,
    pub total_exec_time_ms: f64,
}

/// Table schema information.
#[derive(Debug, Clone, Serialize)]
pub struct TableSchema {
    pub table_name: String,
    pub columns: Vec<ColumnInfo>,
    pub indexes: Vec<IndexInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub is_nullable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexInfo {
    pub index_name: String,
    pub columns: String,
    pub is_unique: bool,
}

/// Index usage statistics.
#[derive(Debug, Clone, Serialize)]
pub struct IndexUsage {
    pub index_name: String,
    pub table_name: String,
    pub idx_scan: i64,
    pub idx_tup_read: i64,
}

/// Table statistics.
#[derive(Debug, Clone, Serialize)]
pub struct TableStats {
    pub table_name: String,
    pub seq_scan: i64,
    pub idx_scan: i64,
    pub n_live_tup: i64,
    pub n_dead_tup: i64,
}

/// EXPLAIN plan output for a query.
#[derive(Debug, Clone, Serialize)]
pub struct ExplainPlan {
    pub sql: String,
    pub plan_json: serde_json::Value,
}

/// Complete PG context for LLM consumption.
#[derive(Debug, Clone, Serialize)]
pub struct PgContext {
    pub pg_version: String,
    pub non_default_settings: Vec<PgSetting>,
    pub top_slow_queries: Vec<SlowQuery>,
    pub stat_statements: Option<Vec<StatStatement>>,
    pub schema: Vec<TableSchema>,
    pub index_usage: Vec<IndexUsage>,
    pub table_stats: Vec<TableStats>,
    pub explain_plans: Vec<ExplainPlan>,
}

/// Connect to the target database and return a client.
pub async fn connect(connection_string: &str) -> Result<Client> {
    let (client, connection) = tokio_postgres::connect(connection_string, NoTls).await?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("PG connection error: {e}");
        }
    });
    Ok(client)
}

/// Collect full PG context from the target database.
pub async fn collect_context(
    client: &Client,
    profile: &WorkloadProfile,
    max_slow_queries: usize,
) -> Result<PgContext> {
    let pg_version = collect_version(client).await?;
    let non_default_settings = collect_settings(client).await?;
    let stat_statements = collect_stat_statements(client).await.ok();
    let schema = collect_schema(client).await?;
    let index_usage = collect_index_usage(client).await?;
    let table_stats = collect_table_stats(client).await?;
    let top_slow_queries = extract_slow_queries(profile, max_slow_queries);
    let explain_plans = collect_explain_plans(client, &top_slow_queries).await;

    Ok(PgContext {
        pg_version,
        non_default_settings,
        top_slow_queries,
        stat_statements,
        schema,
        index_usage,
        table_stats,
        explain_plans,
    })
}

async fn collect_version(client: &Client) -> Result<String> {
    let row = client.query_one("SHOW server_version", &[]).await?;
    Ok(row.get::<_, String>(0))
}

async fn collect_settings(client: &Client) -> Result<Vec<PgSetting>> {
    let rows = client
        .query(
            "SELECT name, setting, unit, source FROM pg_settings \
             WHERE source NOT IN ('default', 'override') \
             ORDER BY name",
            &[],
        )
        .await?;

    Ok(rows
        .iter()
        .map(|row| PgSetting {
            name: row.get(0),
            setting: row.get(1),
            unit: row.get(2),
            source: row.get(3),
        })
        .collect())
}

async fn collect_stat_statements(client: &Client) -> Result<Vec<StatStatement>> {
    let rows = client
        .query(
            "SELECT query, calls, mean_exec_time, total_exec_time \
             FROM pg_stat_statements \
             ORDER BY total_exec_time DESC LIMIT 20",
            &[],
        )
        .await?;

    Ok(rows
        .iter()
        .map(|row| StatStatement {
            query: row.get(0),
            calls: row.get(1),
            mean_exec_time_ms: row.get(2),
            total_exec_time_ms: row.get(3),
        })
        .collect())
}

async fn collect_schema(client: &Client) -> Result<Vec<TableSchema>> {
    // Get all user tables
    let table_rows = client
        .query(
            "SELECT table_name FROM information_schema.tables \
             WHERE table_schema = 'public' AND table_type = 'BASE TABLE' \
             ORDER BY table_name",
            &[],
        )
        .await?;

    let mut schemas = Vec::new();
    for table_row in &table_rows {
        let table_name: String = table_row.get(0);

        // Get columns
        let col_rows = client
            .query(
                "SELECT column_name, data_type, is_nullable \
                 FROM information_schema.columns \
                 WHERE table_schema = 'public' AND table_name = $1 \
                 ORDER BY ordinal_position",
                &[&table_name],
            )
            .await?;

        let columns: Vec<ColumnInfo> = col_rows
            .iter()
            .map(|r| ColumnInfo {
                name: r.get(0),
                data_type: r.get(1),
                is_nullable: r.get::<_, String>(2) == "YES",
            })
            .collect();

        // Get indexes
        let idx_rows = client
            .query(
                "SELECT indexname, indexdef \
                 FROM pg_indexes \
                 WHERE schemaname = 'public' AND tablename = $1",
                &[&table_name],
            )
            .await?;

        let indexes: Vec<IndexInfo> = idx_rows
            .iter()
            .map(|r| {
                let indexdef: String = r.get(1);
                IndexInfo {
                    index_name: r.get(0),
                    columns: indexdef,
                    is_unique: indexdef.to_uppercase().contains("UNIQUE"),
                }
            })
            .collect();

        schemas.push(TableSchema {
            table_name,
            columns,
            indexes,
        });
    }

    Ok(schemas)
}

async fn collect_index_usage(client: &Client) -> Result<Vec<IndexUsage>> {
    let rows = client
        .query(
            "SELECT indexrelname, relname, idx_scan, idx_tup_read \
             FROM pg_stat_user_indexes \
             ORDER BY idx_scan DESC",
            &[],
        )
        .await?;

    Ok(rows
        .iter()
        .map(|row| IndexUsage {
            index_name: row.get(0),
            table_name: row.get(1),
            idx_scan: row.get(2),
            idx_tup_read: row.get(3),
        })
        .collect())
}

async fn collect_table_stats(client: &Client) -> Result<Vec<TableStats>> {
    let rows = client
        .query(
            "SELECT relname, seq_scan, idx_scan, n_live_tup, n_dead_tup \
             FROM pg_stat_user_tables \
             ORDER BY seq_scan DESC",
            &[],
        )
        .await?;

    Ok(rows
        .iter()
        .map(|row| TableStats {
            table_name: row.get(0),
            seq_scan: row.get(1),
            idx_scan: row.get(2),
            n_live_tup: row.get(3),
            n_dead_tup: row.get(4),
        })
        .collect())
}

/// Extract the slowest unique queries from the workload profile.
fn extract_slow_queries(profile: &WorkloadProfile, max: usize) -> Vec<SlowQuery> {
    use std::collections::HashMap;

    let mut query_stats: HashMap<String, (u64, usize)> = HashMap::new();
    for session in &profile.sessions {
        for query in &session.queries {
            let entry = query_stats
                .entry(query.sql.clone())
                .or_insert((0, 0));
            if query.duration_us > entry.0 {
                entry.0 = query.duration_us;
            }
            entry.1 += 1;
        }
    }

    let mut slow: Vec<SlowQuery> = query_stats
        .into_iter()
        .map(|(sql, (duration_us, occurrences))| SlowQuery {
            sql,
            duration_us,
            occurrences,
        })
        .collect();

    slow.sort_by(|a, b| b.duration_us.cmp(&a.duration_us));
    slow.truncate(max);
    slow
}

/// Run EXPLAIN for slow queries. Failures are silently skipped
/// (query might reference tables that don't exist on target).
async fn collect_explain_plans(client: &Client, queries: &[SlowQuery]) -> Vec<ExplainPlan> {
    let mut plans = Vec::new();
    for q in queries {
        // Skip non-SELECT queries and queries with bind params
        let sql_upper = q.sql.to_uppercase();
        if !sql_upper.starts_with("SELECT") || q.sql.contains('$') {
            continue;
        }
        let explain_sql = format!("EXPLAIN (FORMAT JSON, COSTS) {}", q.sql);
        match client.query_one(&explain_sql, &[]).await {
            Ok(row) => {
                let plan_json: serde_json::Value = row.get(0);
                plans.push(ExplainPlan {
                    sql: q.sql.clone(),
                    plan_json,
                });
            }
            Err(_) => {
                // Skip queries that can't be explained (missing tables, etc.)
            }
        }
    }
    plans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Metadata, Query, QueryKind, Session};

    #[test]
    fn test_extract_slow_queries() {
        let profile = WorkloadProfile {
            version: 2,
            captured_at: chrono::Utc::now(),
            source_host: "localhost".into(),
            pg_version: "16".into(),
            capture_method: "test".into(),
            sessions: vec![Session {
                id: 1,
                user: "app".into(),
                database: "test".into(),
                queries: vec![
                    Query { sql: "SELECT 1".into(), start_offset_us: 0, duration_us: 100, kind: QueryKind::Select, transaction_id: None },
                    Query { sql: "SELECT * FROM big_table".into(), start_offset_us: 1000, duration_us: 50000, kind: QueryKind::Select, transaction_id: None },
                    Query { sql: "SELECT 1".into(), start_offset_us: 2000, duration_us: 200, kind: QueryKind::Select, transaction_id: None },
                    Query { sql: "SELECT * FROM big_table".into(), start_offset_us: 3000, duration_us: 60000, kind: QueryKind::Select, transaction_id: None },
                ],
            }],
            metadata: Metadata { total_queries: 4, total_sessions: 1, capture_duration_us: 4000 },
        };

        let slow = extract_slow_queries(&profile, 5);
        assert_eq!(slow.len(), 2); // 2 distinct queries
        assert_eq!(slow[0].sql, "SELECT * FROM big_table");
        assert_eq!(slow[0].duration_us, 60000); // max duration
        assert_eq!(slow[0].occurrences, 2);
        assert_eq!(slow[1].sql, "SELECT 1");
        assert_eq!(slow[1].occurrences, 2);
    }
}
```

**Step 2: Add `pub mod context;` to `src/tuner/mod.rs`**

**Step 3: Run tests**

Run: `cargo test --lib tuner::context`
Expected: 1 test passes

**Step 4: Commit**

```bash
git add src/tuner/context.rs src/tuner/mod.rs
git commit -m "feat(tuner): add PG context collector with schema, settings, and EXPLAIN introspection"
```

---

### Task 4: Recommendation Application

**Files:**
- Create: `src/tuner/apply.rs`
- Modify: `src/tuner/mod.rs` (add `pub mod apply;`)

**Step 1: Create `src/tuner/apply.rs`**

This module applies validated recommendations to the target database and tracks rollback SQL.

```rust
use anyhow::Result;
use tokio_postgres::Client;

use super::types::{AppliedChange, Recommendation};

/// Apply a single recommendation to the target database.
/// Returns an AppliedChange with success/failure status and rollback SQL.
pub async fn apply_recommendation(
    client: &Client,
    rec: &Recommendation,
) -> AppliedChange {
    match rec {
        Recommendation::ConfigChange {
            parameter,
            current_value,
            recommended_value,
            ..
        } => {
            let sql = format!("ALTER SYSTEM SET {} = '{}'", parameter, recommended_value);
            let reload_sql = "SELECT pg_reload_conf()";
            let rollback = format!("ALTER SYSTEM SET {} = '{}'", parameter, current_value);

            match execute_sql(client, &sql).await {
                Ok(()) => {
                    // Reload to apply the change
                    let _ = execute_sql(client, reload_sql).await;
                    AppliedChange {
                        recommendation: rec.clone(),
                        success: true,
                        error: None,
                        rollback_sql: Some(rollback),
                    }
                }
                Err(e) => AppliedChange {
                    recommendation: rec.clone(),
                    success: false,
                    error: Some(e.to_string()),
                    rollback_sql: None,
                },
            }
        }
        Recommendation::CreateIndex { sql, .. } => {
            // Extract index name for rollback
            let rollback = extract_index_name(sql)
                .map(|name| format!("DROP INDEX IF EXISTS {}", name));

            match execute_sql(client, sql).await {
                Ok(()) => AppliedChange {
                    recommendation: rec.clone(),
                    success: true,
                    error: None,
                    rollback_sql: rollback,
                },
                Err(e) => AppliedChange {
                    recommendation: rec.clone(),
                    success: false,
                    error: Some(e.to_string()),
                    rollback_sql: None,
                },
            }
        }
        Recommendation::QueryRewrite { .. } => {
            // Query rewrites are informational — they don't modify the database.
            // The user would need to update their application code.
            AppliedChange {
                recommendation: rec.clone(),
                success: true,
                error: None,
                rollback_sql: None,
            }
        }
        Recommendation::SchemaChange { sql, .. } => {
            match execute_sql(client, sql).await {
                Ok(()) => AppliedChange {
                    recommendation: rec.clone(),
                    success: true,
                    error: None,
                    rollback_sql: None, // Schema changes are harder to auto-rollback
                },
                Err(e) => AppliedChange {
                    recommendation: rec.clone(),
                    success: false,
                    error: Some(e.to_string()),
                    rollback_sql: None,
                },
            }
        }
    }
}

/// Apply a list of recommendations sequentially.
pub async fn apply_all(
    client: &Client,
    recs: &[Recommendation],
) -> Vec<AppliedChange> {
    let mut results = Vec::new();
    for rec in recs {
        let change = apply_recommendation(client, rec).await;
        results.push(change);
    }
    results
}

async fn execute_sql(client: &Client, sql: &str) -> Result<()> {
    client.batch_execute(sql).await?;
    Ok(())
}

/// Extract the index name from a CREATE INDEX statement.
fn extract_index_name(sql: &str) -> Option<String> {
    let upper = sql.to_uppercase();
    let idx = if upper.contains("IF NOT EXISTS") {
        upper.find("IF NOT EXISTS")? + "IF NOT EXISTS".len()
    } else {
        upper.find("INDEX")? + "INDEX".len()
    };
    let rest = sql[idx..].trim();
    let name = rest.split_whitespace().next()?;
    // Skip if the next word is ON (concurrently case)
    if name.to_uppercase() == "CONCURRENTLY" {
        let rest2 = rest["CONCURRENTLY".len()..].trim();
        return rest2.split_whitespace().next().map(|s| s.to_string());
    }
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_index_name() {
        assert_eq!(
            extract_index_name("CREATE INDEX idx_orders_status ON orders (status)"),
            Some("idx_orders_status".into())
        );
        assert_eq!(
            extract_index_name("CREATE INDEX IF NOT EXISTS idx_foo ON bar (baz)"),
            Some("idx_foo".into())
        );
        assert_eq!(
            extract_index_name("CREATE UNIQUE INDEX idx_unique ON t (c)"),
            Some("idx_unique".into())
        );
    }
}
```

**Step 2: Add `pub mod apply;` to `src/tuner/mod.rs`**

**Step 3: Run tests**

Run: `cargo test --lib tuner::apply`
Expected: 1 test passes

**Step 4: Commit**

```bash
git add src/tuner/apply.rs src/tuner/mod.rs
git commit -m "feat(tuner): add recommendation application with rollback tracking"
```

---

### Task 5: TuningAdvisor LLM Integration

**Files:**
- Create: `src/tuner/advisor.rs`
- Modify: `src/tuner/mod.rs` (add `pub mod advisor;`)

**Step 1: Create `src/tuner/advisor.rs`**

Reuses HTTP patterns from `transform/planner.rs` but with a new trait for tuning output.

```rust
use anyhow::{bail, Result};
use serde_json::json;

use crate::transform::analyze::WorkloadAnalysis;
use crate::transform::planner::LlmProvider;

use super::context::PgContext;
use super::types::{Recommendation, TuningIteration};

/// Trait for LLM-based tuning advisors.
#[async_trait::async_trait]
pub trait TuningAdvisor: Send + Sync {
    async fn recommend(
        &self,
        context: &PgContext,
        workload: &WorkloadAnalysis,
        hint: Option<&str>,
        previous: &[TuningIteration],
    ) -> Result<Vec<Recommendation>>;

    fn name(&self) -> &str;
}

/// Configuration for creating a TuningAdvisor.
pub struct AdvisorConfig {
    pub provider: LlmProvider,
    pub api_key: String,
    pub api_url: Option<String>,
    pub model: Option<String>,
}

/// Create a TuningAdvisor from config.
pub fn create_advisor(config: AdvisorConfig) -> Box<dyn TuningAdvisor> {
    match config.provider {
        LlmProvider::Claude => {
            let model = config.model.unwrap_or_else(|| "claude-sonnet-4-20250514".into());
            let url = config.api_url.unwrap_or_else(|| "https://api.anthropic.com".into());
            Box::new(ClaudeAdvisor {
                api_key: config.api_key,
                model,
                base_url: url,
            })
        }
        LlmProvider::OpenAi => {
            let model = config.model.unwrap_or_else(|| "gpt-4o".into());
            let url = config.api_url.unwrap_or_else(|| "https://api.openai.com".into());
            Box::new(OpenAiAdvisor {
                api_key: config.api_key,
                model,
                base_url: url,
            })
        }
        LlmProvider::Ollama => {
            let model = config.model.unwrap_or_else(|| "llama3".into());
            let url = config.api_url.unwrap_or_else(|| "http://localhost:11434".into());
            Box::new(OllamaAdvisor {
                model,
                base_url: url,
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Prompt construction
// ---------------------------------------------------------------------------

pub fn build_system_prompt() -> String {
    r#"You are a PostgreSQL tuning expert. Given a database's current configuration, schema, query performance, and workload patterns, recommend changes to improve performance.

You have four recommendation tools:
1. config_change — Change a PostgreSQL configuration parameter
2. create_index — Create a new index to speed up queries
3. query_rewrite — Suggest an optimized version of a slow query
4. schema_change — Suggest a schema modification (ALTER TABLE, etc.)

Guidelines:
- Prioritize changes with the highest expected performance impact
- For config changes, specify the parameter name, current value, and recommended value
- For indexes, provide the complete CREATE INDEX statement
- For query rewrites, show the original and optimized SQL side by side
- Consider previous iteration results — do not repeat changes that were ineffective
- Provide clear rationale for each recommendation
- Limit to 3-5 recommendations per iteration (focus on highest impact)"#.into()
}

pub fn build_user_message(
    context: &PgContext,
    workload: &WorkloadAnalysis,
    hint: Option<&str>,
    previous: &[TuningIteration],
) -> String {
    let mut msg = String::new();

    msg.push_str("## Database Context\n");
    msg.push_str(&serde_json::to_string_pretty(context).unwrap_or_default());
    msg.push_str("\n\n");

    msg.push_str("## Workload Summary\n");
    msg.push_str(&serde_json::to_string_pretty(workload).unwrap_or_default());
    msg.push_str("\n\n");

    if let Some(h) = hint {
        msg.push_str("## User Hint\n");
        msg.push_str(h);
        msg.push_str("\n\n");
    }

    if !previous.is_empty() {
        msg.push_str("## Previous Iterations\n");
        for iter in previous {
            msg.push_str(&format!(
                "Iteration {}: {} recommendations applied.\n",
                iter.iteration,
                iter.applied.iter().filter(|a| a.success).count()
            ));
            msg.push_str(&iter.llm_feedback);
            msg.push_str("\n");
        }
        msg.push_str("\n");
    }

    msg.push_str("## Instructions\n");
    msg.push_str("Analyze the database context and workload, then use the tools to recommend tuning changes.\n");

    msg
}

fn tool_schema() -> serde_json::Value {
    json!([
        {
            "name": "config_change",
            "description": "Recommend a PostgreSQL configuration parameter change",
            "input_schema": {
                "type": "object",
                "properties": {
                    "parameter": { "type": "string", "description": "PostgreSQL parameter name" },
                    "current_value": { "type": "string", "description": "Current value" },
                    "recommended_value": { "type": "string", "description": "Recommended new value" },
                    "rationale": { "type": "string", "description": "Why this change helps" }
                },
                "required": ["parameter", "current_value", "recommended_value", "rationale"]
            }
        },
        {
            "name": "create_index",
            "description": "Recommend creating a new database index",
            "input_schema": {
                "type": "object",
                "properties": {
                    "table": { "type": "string" },
                    "columns": { "type": "array", "items": { "type": "string" } },
                    "index_type": { "type": "string", "description": "btree, hash, gin, gist (optional)" },
                    "sql": { "type": "string", "description": "Complete CREATE INDEX statement" },
                    "rationale": { "type": "string" }
                },
                "required": ["table", "columns", "sql", "rationale"]
            }
        },
        {
            "name": "query_rewrite",
            "description": "Suggest an optimized version of a slow query",
            "input_schema": {
                "type": "object",
                "properties": {
                    "original_sql": { "type": "string" },
                    "rewritten_sql": { "type": "string" },
                    "rationale": { "type": "string" }
                },
                "required": ["original_sql", "rewritten_sql", "rationale"]
            }
        },
        {
            "name": "schema_change",
            "description": "Suggest a schema modification",
            "input_schema": {
                "type": "object",
                "properties": {
                    "sql": { "type": "string", "description": "ALTER TABLE or other DDL" },
                    "description": { "type": "string" },
                    "rationale": { "type": "string" }
                },
                "required": ["sql", "description", "rationale"]
            }
        }
    ])
}

fn parse_tool_call(name: &str, input: &serde_json::Value) -> Option<Recommendation> {
    match name {
        "config_change" => Some(Recommendation::ConfigChange {
            parameter: input["parameter"].as_str()?.into(),
            current_value: input["current_value"].as_str()?.into(),
            recommended_value: input["recommended_value"].as_str()?.into(),
            rationale: input["rationale"].as_str().unwrap_or("").into(),
        }),
        "create_index" => Some(Recommendation::CreateIndex {
            table: input["table"].as_str()?.into(),
            columns: input["columns"]
                .as_array()?
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            index_type: input["index_type"].as_str().map(String::from),
            sql: input["sql"].as_str()?.into(),
            rationale: input["rationale"].as_str().unwrap_or("").into(),
        }),
        "query_rewrite" => Some(Recommendation::QueryRewrite {
            original_sql: input["original_sql"].as_str()?.into(),
            rewritten_sql: input["rewritten_sql"].as_str()?.into(),
            rationale: input["rationale"].as_str().unwrap_or("").into(),
        }),
        "schema_change" => Some(Recommendation::SchemaChange {
            sql: input["sql"].as_str()?.into(),
            description: input["description"].as_str().unwrap_or("").into(),
            rationale: input["rationale"].as_str().unwrap_or("").into(),
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Claude Advisor
// ---------------------------------------------------------------------------

struct ClaudeAdvisor {
    api_key: String,
    model: String,
    base_url: String,
}

#[async_trait::async_trait]
impl TuningAdvisor for ClaudeAdvisor {
    async fn recommend(
        &self,
        context: &PgContext,
        workload: &WorkloadAnalysis,
        hint: Option<&str>,
        previous: &[TuningIteration],
    ) -> Result<Vec<Recommendation>> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;

        let body = json!({
            "model": self.model,
            "max_tokens": 4096,
            "system": build_system_prompt(),
            "tools": tool_schema(),
            "messages": [{ "role": "user", "content": build_user_message(context, workload, hint, previous) }]
        });

        let resp = client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Claude API error {status}: {text}");
        }

        let data: serde_json::Value = resp.json().await?;
        let mut recs = Vec::new();

        if let Some(content) = data["content"].as_array() {
            for block in content {
                if block["type"] == "tool_use" {
                    if let Some(name) = block["name"].as_str() {
                        if let Some(rec) = parse_tool_call(name, &block["input"]) {
                            recs.push(rec);
                        }
                    }
                }
            }
        }

        Ok(recs)
    }

    fn name(&self) -> &str {
        "Claude"
    }
}

// ---------------------------------------------------------------------------
// OpenAI Advisor
// ---------------------------------------------------------------------------

struct OpenAiAdvisor {
    api_key: String,
    model: String,
    base_url: String,
}

#[async_trait::async_trait]
impl TuningAdvisor for OpenAiAdvisor {
    async fn recommend(
        &self,
        context: &PgContext,
        workload: &WorkloadAnalysis,
        hint: Option<&str>,
        previous: &[TuningIteration],
    ) -> Result<Vec<Recommendation>> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;

        let tools: Vec<serde_json::Value> = tool_schema()
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t["name"],
                        "description": t["description"],
                        "parameters": t["input_schema"]
                    }
                })
            })
            .collect();

        let body = json!({
            "model": self.model,
            "max_tokens": 4096,
            "messages": [
                { "role": "system", "content": build_system_prompt() },
                { "role": "user", "content": build_user_message(context, workload, hint, previous) }
            ],
            "tools": tools
        });

        let resp = client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("OpenAI API error {status}: {text}");
        }

        let data: serde_json::Value = resp.json().await?;
        let mut recs = Vec::new();

        if let Some(tool_calls) = data["choices"][0]["message"]["tool_calls"].as_array() {
            for tc in tool_calls {
                if let (Some(name), Some(args_str)) = (
                    tc["function"]["name"].as_str(),
                    tc["function"]["arguments"].as_str(),
                ) {
                    if let Ok(args) = serde_json::from_str::<serde_json::Value>(args_str) {
                        if let Some(rec) = parse_tool_call(name, &args) {
                            recs.push(rec);
                        }
                    }
                }
            }
        }

        Ok(recs)
    }

    fn name(&self) -> &str {
        "OpenAI"
    }
}

// ---------------------------------------------------------------------------
// Ollama Advisor
// ---------------------------------------------------------------------------

struct OllamaAdvisor {
    model: String,
    base_url: String,
}

#[async_trait::async_trait]
impl TuningAdvisor for OllamaAdvisor {
    async fn recommend(
        &self,
        context: &PgContext,
        workload: &WorkloadAnalysis,
        hint: Option<&str>,
        previous: &[TuningIteration],
    ) -> Result<Vec<Recommendation>> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()?;

        let prompt = format!(
            "{}\n\n{}\n\nRespond with a JSON array of recommendation objects. \
             Each object must have a \"type\" field (config_change, create_index, \
             query_rewrite, or schema_change) and the corresponding fields.",
            build_system_prompt(),
            build_user_message(context, workload, hint, previous),
        );

        let body = json!({
            "model": self.model,
            "prompt": prompt,
            "format": "json",
            "stream": false,
        });

        let resp = client
            .post(format!("{}/api/generate", self.base_url))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Ollama API error {status}: {text}");
        }

        let data: serde_json::Value = resp.json().await?;
        let response_text = data["response"].as_str().unwrap_or("[]");

        let recs: Vec<Recommendation> = serde_json::from_str(response_text)
            .unwrap_or_default();

        Ok(recs)
    }

    fn name(&self) -> &str {
        "Ollama"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tool_calls() {
        let input = json!({
            "parameter": "shared_buffers",
            "current_value": "128MB",
            "recommended_value": "1GB",
            "rationale": "More memory for caching"
        });
        let rec = parse_tool_call("config_change", &input).unwrap();
        assert!(matches!(rec, Recommendation::ConfigChange { .. }));

        let input = json!({
            "table": "orders",
            "columns": ["status", "created_at"],
            "sql": "CREATE INDEX idx_orders_status ON orders (status, created_at)",
            "rationale": "Speed up status queries"
        });
        let rec = parse_tool_call("create_index", &input).unwrap();
        assert!(matches!(rec, Recommendation::CreateIndex { .. }));

        assert!(parse_tool_call("unknown_tool", &json!({})).is_none());
    }

    #[test]
    fn test_build_system_prompt() {
        let prompt = build_system_prompt();
        assert!(prompt.contains("PostgreSQL tuning expert"));
        assert!(prompt.contains("config_change"));
        assert!(prompt.contains("create_index"));
        assert!(prompt.contains("query_rewrite"));
        assert!(prompt.contains("schema_change"));
    }
}
```

**Step 2: Add `pub mod advisor;` to `src/tuner/mod.rs`**

**Step 3: Run tests**

Run: `cargo test --lib tuner::advisor`
Expected: 2 tests pass

**Step 4: Commit**

```bash
git add src/tuner/advisor.rs src/tuner/mod.rs
git commit -m "feat(tuner): add TuningAdvisor trait with Claude/OpenAI/Ollama providers"
```

---

### Task 6: Tuning Orchestrator

**Files:**
- Modify: `src/tuner/mod.rs` (add the orchestrator logic)

**Step 1: Replace `src/tuner/mod.rs` with orchestrator + module declarations**

```rust
pub mod advisor;
pub mod apply;
pub mod context;
pub mod safety;
pub mod types;

use anyhow::{bail, Result};

use crate::compare;
use crate::profile::WorkloadProfile;
use crate::replay::{self, ReplayMode};
use crate::transform::analyze;

use self::advisor::{create_advisor, AdvisorConfig};
use self::apply::apply_all;
use self::context::{collect_context, connect};
use self::safety::{check_production_hostname, validate_recommendations};
use self::types::*;

/// Run the tuning loop. Returns a TuningReport.
///
/// If `config.apply` is false (dry-run), only the first iteration's
/// recommendations are generated and printed — nothing is applied.
pub async fn run_tuning(config: &TuningConfig) -> Result<TuningReport> {
    // 1. Safety: check production hostname
    check_production_hostname(&config.target, config.force)?;

    // 2. Load workload profile
    let profile = crate::profile::load_profile(&config.workload_path)?;

    // 3. Analyze workload
    let workload_analysis = analyze::analyze_workload(&profile);

    // 4. Resolve API key
    let api_key = config
        .api_key
        .clone()
        .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
        .or_else(|| std::env::var("OPENAI_API_KEY").ok())
        .unwrap_or_default();

    let provider: crate::transform::planner::LlmProvider = config.provider.parse()?;

    // 5. Create advisor
    let advisor = create_advisor(AdvisorConfig {
        provider,
        api_key,
        api_url: config.api_url.clone(),
        model: config.model.clone(),
    });

    // 6. Connect to target
    let client = connect(&config.target).await?;

    // 7. Collect baseline: replay once to get baseline metrics
    let replay_mode = if config.read_only {
        ReplayMode::ReadOnly
    } else {
        ReplayMode::ReadWrite
    };

    println!("  Collecting baseline replay...");
    let baseline_results = replay::session::run_replay(
        &profile,
        &config.target,
        replay_mode,
        config.speed,
    )
    .await?;

    let baseline_report = compare::compute_comparison(&profile, &baseline_results, 20.0);

    let mut iterations: Vec<TuningIteration> = Vec::new();
    let mut all_changes: Vec<AppliedChange> = Vec::new();

    for i in 1..=config.max_iterations {
        println!("\n  === Tuning Iteration {}/{} ===", i, config.max_iterations);

        // Collect context (re-collect each iteration to see impact of changes)
        println!("  Collecting PG context...");
        let context = collect_context(&client, &profile, 10).await?;

        // Call LLM
        println!("  Requesting recommendations from {}...", advisor.name());
        let recommendations = advisor
            .recommend(&context, &workload_analysis, config.hint.as_deref(), &iterations)
            .await?;

        if recommendations.is_empty() {
            println!("  No recommendations from LLM. Stopping.");
            break;
        }

        println!("  Received {} recommendations:", recommendations.len());
        for (j, rec) in recommendations.iter().enumerate() {
            print_recommendation(j + 1, rec);
        }

        // Validate safety
        let (safe_recs, rejected) = validate_recommendations(&recommendations);
        if !rejected.is_empty() {
            println!("\n  Rejected {} recommendations:", rejected.len());
            for (rec, reason) in &rejected {
                println!("    - {}: {}", rec_summary(rec), reason);
            }
        }

        if safe_recs.is_empty() {
            println!("  All recommendations rejected by safety layer. Stopping.");
            iterations.push(TuningIteration {
                iteration: i,
                recommendations,
                applied: vec![],
                comparison: None,
                llm_feedback: "All recommendations rejected by safety layer.".into(),
            });
            break;
        }

        // Dry-run: stop after printing
        if !config.apply {
            println!("\n  Dry-run mode — not applying changes. Use --apply to execute.");
            iterations.push(TuningIteration {
                iteration: i,
                recommendations,
                applied: vec![],
                comparison: None,
                llm_feedback: "Dry-run — not applied.".into(),
            });
            break;
        }

        // Apply recommendations
        println!("\n  Applying {} recommendations...", safe_recs.len());
        let applied = apply_all(&client, &safe_recs).await;

        let successes = applied.iter().filter(|a| a.success).count();
        let failures = applied.iter().filter(|a| !a.success).count();
        println!("  Applied: {} success, {} failed", successes, failures);

        for a in &applied {
            if !a.success {
                println!("    FAILED: {} — {}", rec_summary(&a.recommendation),
                    a.error.as_deref().unwrap_or("unknown"));
            }
        }

        all_changes.extend(applied.clone());

        if successes == 0 {
            println!("  No changes applied successfully. Stopping.");
            iterations.push(TuningIteration {
                iteration: i,
                recommendations,
                applied,
                comparison: None,
                llm_feedback: "No changes applied successfully.".into(),
            });
            break;
        }

        // Replay after changes
        println!("  Replaying workload...");
        let replay_results = replay::session::run_replay(
            &profile,
            &config.target,
            replay_mode,
            config.speed,
        )
        .await?;

        let iter_report = compare::compute_comparison(&profile, &replay_results, 20.0);

        // Compare vs baseline
        let comparison = ComparisonSummary {
            p50_change_pct: pct_change(baseline_report.source_p50_latency_us, iter_report.replay_p50_latency_us),
            p95_change_pct: pct_change(baseline_report.source_p95_latency_us, iter_report.replay_p95_latency_us),
            p99_change_pct: pct_change(baseline_report.source_p99_latency_us, iter_report.replay_p99_latency_us),
            regressions: iter_report.regressions.len(),
            improvements: 0, // TODO: count improvements
            errors_delta: iter_report.total_errors as i64 - baseline_report.total_errors as i64,
        };

        println!("  Results: p50={:+.1}%, p95={:+.1}%, p99={:+.1}%",
            comparison.p50_change_pct, comparison.p95_change_pct, comparison.p99_change_pct);

        // Build feedback for next iteration
        let feedback = format!(
            "p50: {:+.1}%, p95: {:+.1}%, p99: {:+.1}%, {} regressions, {} errors delta.",
            comparison.p50_change_pct, comparison.p95_change_pct,
            comparison.p99_change_pct, comparison.regressions, comparison.errors_delta,
        );

        // Check for regression — stop if p95 got worse
        let should_stop = comparison.p95_change_pct > 5.0;

        iterations.push(TuningIteration {
            iteration: i,
            recommendations,
            applied,
            comparison: Some(comparison),
            llm_feedback: feedback,
        });

        if should_stop {
            println!("  p95 latency regressed. Stopping early.");
            break;
        }
    }

    // Calculate total improvement
    let total_improvement_pct = iterations
        .last()
        .and_then(|i| i.comparison.as_ref())
        .map(|c| -c.p95_change_pct) // negative change = improvement
        .unwrap_or(0.0);

    let report = TuningReport {
        workload: config.workload_path.display().to_string(),
        target: config.target.clone(),
        provider: config.provider.clone(),
        hint: config.hint.clone(),
        iterations,
        total_improvement_pct,
        all_changes,
    };

    print_tuning_summary(&report);

    Ok(report)
}

fn pct_change(baseline: u64, current: u64) -> f64 {
    if baseline == 0 {
        return 0.0;
    }
    ((current as f64 - baseline as f64) / baseline as f64) * 100.0
}

fn rec_summary(rec: &Recommendation) -> String {
    match rec {
        Recommendation::ConfigChange { parameter, recommended_value, .. } => {
            format!("config: {} = {}", parameter, recommended_value)
        }
        Recommendation::CreateIndex { sql, .. } => {
            let preview: String = sql.chars().take(60).collect();
            format!("index: {}", preview)
        }
        Recommendation::QueryRewrite { original_sql, .. } => {
            let preview: String = original_sql.chars().take(60).collect();
            format!("rewrite: {}", preview)
        }
        Recommendation::SchemaChange { description, .. } => {
            format!("schema: {}", description)
        }
    }
}

fn print_recommendation(num: usize, rec: &Recommendation) {
    match rec {
        Recommendation::ConfigChange { parameter, current_value, recommended_value, rationale } => {
            println!("    {}. [CONFIG] {} = {} -> {}", num, parameter, current_value, recommended_value);
            println!("       Rationale: {}", rationale);
        }
        Recommendation::CreateIndex { sql, rationale, .. } => {
            println!("    {}. [INDEX] {}", num, sql);
            println!("       Rationale: {}", rationale);
        }
        Recommendation::QueryRewrite { original_sql, rewritten_sql, rationale } => {
            let orig_preview: String = original_sql.chars().take(80).collect();
            let new_preview: String = rewritten_sql.chars().take(80).collect();
            println!("    {}. [REWRITE] {} -> {}", num, orig_preview, new_preview);
            println!("       Rationale: {}", rationale);
        }
        Recommendation::SchemaChange { sql, description, rationale } => {
            println!("    {}. [SCHEMA] {} — {}", num, description, sql);
            println!("       Rationale: {}", rationale);
        }
    }
}

fn print_tuning_summary(report: &TuningReport) {
    println!("\n  Tuning Summary");
    println!("  ==============");
    println!("  Workload:       {}", report.workload);
    println!("  Target:         {}", report.target);
    println!("  Provider:       {}", report.provider);
    if let Some(ref hint) = report.hint {
        println!("  Hint:           {}", hint);
    }
    println!("  Iterations:     {}", report.iterations.len());
    println!("  Changes applied: {}", report.all_changes.iter().filter(|c| c.success).count());
    println!("  Total improvement: {:+.1}%", report.total_improvement_pct);
}
```

**Step 2: Run tests**

Run: `cargo test --lib tuner`
Expected: All tuner unit tests pass (types, safety, context, apply, advisor)

Run: `cargo clippy`
Expected: Zero warnings

**Step 3: Commit**

```bash
git add src/tuner/mod.rs
git commit -m "feat(tuner): add TuningOrchestrator with configurable iteration loop"
```

---

### Task 7: CLI Integration

**Files:**
- Modify: `src/cli.rs` (add `Tune` variant and `TuneArgs`)
- Modify: `src/main.rs` (add `cmd_tune()`)

**Step 1: Add TuneArgs to `src/cli.rs`**

Add to the `Commands` enum:

```rust
    /// Run AI-assisted database tuning
    Tune(TuneArgs),
```

Add the TuneArgs struct:

```rust
#[derive(clap::Args)]
pub struct TuneArgs {
    /// Path to workload profile (.wkl)
    #[arg(long)]
    pub workload: PathBuf,

    /// Target PostgreSQL connection string
    #[arg(long)]
    pub target: String,

    /// LLM provider: claude, openai, ollama
    #[arg(long, default_value = "claude")]
    pub provider: String,

    /// API key (or set ANTHROPIC_API_KEY / OPENAI_API_KEY env var)
    #[arg(long)]
    pub api_key: Option<String>,

    /// Override API URL
    #[arg(long)]
    pub api_url: Option<String>,

    /// Override model name
    #[arg(long)]
    pub model: Option<String>,

    /// Maximum tuning iterations
    #[arg(long, default_value_t = 3)]
    pub max_iterations: u32,

    /// Natural language hint for the LLM
    #[arg(long)]
    pub hint: Option<String>,

    /// Apply recommendations (default is dry-run)
    #[arg(long, default_value_t = false)]
    pub apply: bool,

    /// Allow targeting production-looking hostnames
    #[arg(long, default_value_t = false)]
    pub force: bool,

    /// Output JSON report path
    #[arg(long)]
    pub json: Option<PathBuf>,

    /// Replay speed multiplier
    #[arg(long, default_value_t = 1.0)]
    pub speed: f64,

    /// Replay only SELECT queries
    #[arg(long, default_value_t = false)]
    pub read_only: bool,
}
```

**Step 2: Add `cmd_tune()` to `src/main.rs`**

Add the match arm in the main match:
```rust
Commands::Tune(args) => cmd_tune(args).await,
```

Add the function:
```rust
async fn cmd_tune(args: cli::TuneArgs) -> Result<()> {
    let config = pg_retest::tuner::types::TuningConfig {
        workload_path: args.workload,
        target: args.target,
        provider: args.provider,
        api_key: args.api_key,
        api_url: args.api_url,
        model: args.model,
        max_iterations: args.max_iterations,
        hint: args.hint,
        apply: args.apply,
        force: args.force,
        speed: args.speed,
        read_only: args.read_only,
    };

    let report = pg_retest::tuner::run_tuning(&config).await?;

    if let Some(json_path) = args.json {
        let json = serde_json::to_string_pretty(&report)?;
        std::fs::write(&json_path, json)?;
        println!("\n  Report written to {}", json_path.display());
    }

    Ok(())
}
```

**Step 3: Run tests**

Run: `cargo build`
Expected: Compiles successfully

Run: `cargo test`
Expected: All tests pass

**Step 4: Commit**

```bash
git add src/cli.rs src/main.rs
git commit -m "feat(cli): add tune subcommand with dry-run, hint, and multi-provider support"
```

---

### Task 8: Web API — Tuning Endpoints

**Files:**
- Create: `src/web/handlers/tuning.rs`
- Modify: `src/web/handlers/mod.rs` (add `pub mod tuning;`)
- Modify: `src/web/ws.rs` (add tuning WsMessage variants)
- Modify: `src/web/routes.rs` (add tuning routes)

**Step 1: Add WsMessage variants to `src/web/ws.rs`**

Add to the `WsMessage` enum:

```rust
    // Tuning events
    TuningIterationStarted { task_id: String, iteration: u32 },
    TuningRecommendations { task_id: String, iteration: u32, count: usize },
    TuningChangeApplied { task_id: String, iteration: u32, success: bool, summary: String },
    TuningReplayCompleted { task_id: String, iteration: u32, improvement_pct: f64 },
    TuningCompleted { task_id: String, total_improvement_pct: f64, iterations_completed: u32 },
```

**Step 2: Create `src/web/handlers/tuning.rs`**

```rust
use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;

use crate::web::state::AppState;
use crate::web::ws::WsMessage;

#[derive(Deserialize)]
pub struct StartTuningRequest {
    pub workload_id: String,
    pub target: String,
    pub provider: Option<String>,
    pub api_key: Option<String>,
    pub api_url: Option<String>,
    pub model: Option<String>,
    pub max_iterations: Option<u32>,
    pub hint: Option<String>,
    pub apply: Option<bool>,
    pub speed: Option<f64>,
    pub read_only: Option<bool>,
}

pub async fn start_tuning(
    State(state): State<AppState>,
    Json(req): Json<StartTuningRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Check if tuning is already running
    if state.tasks.has_running("tuning").await {
        return Err(StatusCode::CONFLICT);
    }

    // Resolve workload path
    let wkl_path = state.data_dir.join("workloads").join(&req.workload_id);
    if !wkl_path.exists() {
        return Err(StatusCode::NOT_FOUND);
    }

    let config = crate::tuner::types::TuningConfig {
        workload_path: wkl_path,
        target: req.target.clone(),
        provider: req.provider.unwrap_or_else(|| "claude".into()),
        api_key: req.api_key,
        api_url: req.api_url,
        model: req.model,
        max_iterations: req.max_iterations.unwrap_or(3),
        hint: req.hint,
        apply: req.apply.unwrap_or(false),
        force: false, // Web UI never allows force
        speed: req.speed.unwrap_or(1.0),
        read_only: req.read_only.unwrap_or(false),
    };

    let state_clone = state.clone();
    let task_id = state
        .tasks
        .clone()
        .spawn("tuning", &format!("Tune {}", req.workload_id), move |_cancel, task_id| {
            tokio::spawn(async move {
                match crate::tuner::run_tuning(&config).await {
                    Ok(report) => {
                        let total = report.total_improvement_pct;
                        let iters = report.iterations.len() as u32;
                        state_clone.broadcast(WsMessage::TuningCompleted {
                            task_id,
                            total_improvement_pct: total,
                            iterations_completed: iters,
                        });
                    }
                    Err(e) => {
                        state_clone.broadcast(WsMessage::Error {
                            message: format!("Tuning failed: {e}"),
                        });
                    }
                }
            })
        })
        .await;

    Ok(Json(serde_json::json!({ "task_id": task_id })))
}

pub async fn get_tuning_status(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match state.tasks.get(&id).await {
        Some(info) => Ok(Json(serde_json::json!({
            "task_id": info.id,
            "status": if info.running { "running" } else { "completed" },
            "label": info.label,
        }))),
        None => Err(StatusCode::NOT_FOUND),
    }
}

pub async fn cancel_tuning(
    State(state): State<AppState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if state.tasks.cancel(&id).await {
        Ok(Json(serde_json::json!({ "cancelled": true })))
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}
```

**Step 3: Add `pub mod tuning;` to `src/web/handlers/mod.rs`**

**Step 4: Add tuning routes to `src/web/routes.rs`**

Add alongside existing transform routes:
```rust
// Tuning
.route("/tuning/start", post(handlers::tuning::start_tuning))
.route("/tuning/{id}", get(handlers::tuning::get_tuning_status))
.route("/tuning/{id}/cancel", post(handlers::tuning::cancel_tuning))
```

**Step 5: Run tests**

Run: `cargo build`
Expected: Compiles successfully

Run: `cargo test`
Expected: All tests pass

**Step 6: Commit**

```bash
git add src/web/handlers/tuning.rs src/web/handlers/mod.rs src/web/ws.rs src/web/routes.rs
git commit -m "feat(web): add tuning API endpoints and WebSocket message types"
```

---

### Task 9: Web UI — Tuning Page

**Files:**
- Create: `src/web/static/js/pages/tuning.js`
- Modify: `src/web/static/index.html` (add tuning page section)
- Modify: `src/web/static/js/app.js` (add tuning nav item)

**Step 1: Create `src/web/static/js/pages/tuning.js`**

Alpine.js component with:
- Configuration panel: workload selector, target input, provider/key/model, hint textarea, max iterations slider
- Start/Cancel buttons
- Iteration timeline: expandable cards showing recommendations, applied status, metrics
- Final summary: total improvement, all changes

**Step 2: Add tuning section to `src/web/static/index.html`**

Add a new section with `x-show="page === 'tuning'"` containing the tuning component.
Add script tag: `<script src="/js/pages/tuning.js"></script>`

**Step 3: Add tuning nav item to `src/web/static/js/app.js`**

Add to the navigation array: `{ id: 'tuning', label: 'Tuning', icon: '...' }`

**Step 4: Run tests**

Run: `cargo build`
Expected: Compiles (embedded static files)

**Step 5: Commit**

```bash
git add src/web/static/js/pages/tuning.js src/web/static/index.html src/web/static/js/app.js
git commit -m "feat(web): add Tuning page with iteration timeline UI"
```

---

### Task 10: Integration Tests

**Files:**
- Create: `tests/tuner_test.rs`

**Step 1: Write integration tests**

Tests that can run without an LLM API or live PG (pure logic tests):

```rust
use pg_retest::tuner::types::*;
use pg_retest::tuner::safety::*;

#[test]
fn test_safety_validates_allowlist() {
    let recs = vec![
        Recommendation::ConfigChange {
            parameter: "shared_buffers".into(),
            current_value: "128MB".into(),
            recommended_value: "1GB".into(),
            rationale: "".into(),
        },
        Recommendation::ConfigChange {
            parameter: "data_directory".into(),
            current_value: "/data".into(),
            recommended_value: "/tmp".into(),
            rationale: "".into(),
        },
    ];
    let (safe, rejected) = validate_recommendations(&recs);
    assert_eq!(safe.len(), 1);
    assert_eq!(rejected.len(), 1);
}

#[test]
fn test_production_hostname_blocked() {
    assert!(check_production_hostname("host=production.db", false).is_err());
    assert!(check_production_hostname("host=test.db", false).is_ok());
    assert!(check_production_hostname("host=production.db", true).is_ok());
}

#[test]
fn test_tuning_report_json_output() {
    let report = TuningReport {
        workload: "test.wkl".into(),
        target: "postgresql://localhost/test".into(),
        provider: "claude".into(),
        hint: Some("focus on reads".into()),
        iterations: vec![TuningIteration {
            iteration: 1,
            recommendations: vec![Recommendation::ConfigChange {
                parameter: "work_mem".into(),
                current_value: "4MB".into(),
                recommended_value: "64MB".into(),
                rationale: "Large sorts".into(),
            }],
            applied: vec![AppliedChange {
                recommendation: Recommendation::ConfigChange {
                    parameter: "work_mem".into(),
                    current_value: "4MB".into(),
                    recommended_value: "64MB".into(),
                    rationale: "Large sorts".into(),
                },
                success: true,
                error: None,
                rollback_sql: Some("ALTER SYSTEM SET work_mem = '4MB'".into()),
            }],
            comparison: Some(ComparisonSummary {
                p50_change_pct: -5.0,
                p95_change_pct: -12.0,
                p99_change_pct: -8.0,
                regressions: 0,
                improvements: 3,
                errors_delta: 0,
            }),
            llm_feedback: "p95 improved by 12%".into(),
        }],
        total_improvement_pct: 12.0,
        all_changes: vec![],
    };

    let json = serde_json::to_string_pretty(&report).unwrap();
    let parsed: TuningReport = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.iterations.len(), 1);
    assert_eq!(parsed.total_improvement_pct, 12.0);
    assert!(parsed.iterations[0].comparison.as_ref().unwrap().p95_change_pct < 0.0);
}

#[test]
fn test_blocked_sql_operations() {
    let recs = vec![
        Recommendation::SchemaChange {
            sql: "DROP TABLE users".into(),
            description: "Remove".into(),
            rationale: "".into(),
        },
        Recommendation::SchemaChange {
            sql: "ALTER TABLE users ADD COLUMN age int".into(),
            description: "Add column".into(),
            rationale: "".into(),
        },
        Recommendation::CreateIndex {
            table: "orders".into(),
            columns: vec!["status".into()],
            index_type: None,
            sql: "CREATE INDEX idx_status ON orders (status)".into(),
            rationale: "".into(),
        },
    ];
    let (safe, rejected) = validate_recommendations(&recs);
    assert_eq!(safe.len(), 2); // ADD COLUMN + CREATE INDEX
    assert_eq!(rejected.len(), 1); // DROP TABLE
}
```

**Step 2: Run tests**

Run: `cargo test --test tuner_test`
Expected: 4 tests pass

**Step 3: Commit**

```bash
git add tests/tuner_test.rs
git commit -m "test: add integration tests for tuner safety, report serialization"
```

---

### Task 11: Final Verification and CLAUDE.md

**Files:**
- Modify: `CLAUDE.md` (add tuner module documentation)

**Step 1: Run full test suite**

Run: `cargo fmt && cargo clippy && cargo test`
Expected: All tests pass, zero warnings

**Step 2: Update CLAUDE.md**

Add tuner modules to the key modules list:
```
- `tuner` — AI-assisted tuning orchestrator (configurable loop: context → LLM → safety → apply → replay → compare)
- `tuner::types` — Recommendation, TuningConfig, TuningIteration, TuningReport
- `tuner::context` — PG introspection (pg_settings, schema, pg_stat_statements, EXPLAIN plans)
- `tuner::advisor` — TuningAdvisor trait with Claude/OpenAI/Ollama providers
- `tuner::safety` — Parameter allowlist, blocked operations, production hostname check
- `tuner::apply` — Recommendation application with rollback tracking
- `web::handlers::tuning` — Tuning API endpoints (start, status, cancel)
```

Update CLI subcommand count from 10 to 11.

Update milestone status: change M5 from "Design complete" to "Complete".

Add tuner-specific gotchas:
```
- Tuner: default is dry-run (--apply required to execute). Safety allowlist blocks ~50 dangerous PG params.
- Tuner: baseline is collected via replay before any tuning iteration (comparison is always vs. baseline, not vs. source timing).
- Tuner: pg_stat_statements is optional — if the extension isn't installed, stat_statements will be None.
- Tuner: EXPLAIN is only run for SELECT queries without bind parameters (queries with $1 are skipped).
- Tuner: production hostname check blocks targets containing "prod", "production", "primary", "master", "main" without --force.
```

**Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: update CLAUDE.md with tuner module documentation"
```

---

### Verification Checklist

After all tasks:
- `cargo fmt` — no changes
- `cargo clippy` — zero warnings
- `cargo test` — all tests pass (should be 210+)
- `pg-retest tune --help` — shows all flags
- `pg-retest tune --workload test.wkl --target "..." --dry-run` — generates recommendations without applying
- Web dashboard shows Tuning page at `/` → navigate to Tuning
