# M4: Cross-Database Capture Design (MySQL First)

## Overview
Capture workloads from MySQL (and later Oracle, MariaDB, SQL Server) and transform SQL into PostgreSQL-compatible syntax for replay.

## MySQL Log Sources

### General Query Log
- Location: Set via `general_log_file`
- Format: `timestamp thread_id command_type argument`
- Limitation: No timing information
- Use case: Workload shape analysis, not performance benchmarking

### Slow Query Log (Preferred)
- Location: Set via `slow_query_log_file`
- Format: Includes `Query_time`, `Lock_time`, `Rows_sent`, `Rows_examined`
- Setup: Set `long_query_time=0` to capture all queries
- Advantage: Has timing data for realistic replay

## SQL Transformation Pipeline

### Trait Design
```rust
pub trait SqlTransformer: Send + Sync {
    fn transform(&self, sql: &str) -> TransformResult;
}

pub enum TransformResult {
    Transformed(String),
    Skipped { reason: String },
    Unchanged,
}
```

Composable chain:
```rust
pub struct TransformPipeline {
    transformers: Vec<Box<dyn SqlTransformer>>,
}
```

### Transform Approach: Regex-based v1
Regex-based transformations cover 80-90% of real-world MySQL queries. The `sqlparser` crate is the upgrade path for complex cases.

### Key Transforms

| MySQL | PostgreSQL | Complexity |
|-------|-----------|-----------|
| `` `identifier` `` | `"identifier"` | Simple regex |
| `LIMIT offset, count` | `LIMIT count OFFSET offset` | Regex |
| `IFNULL(a, b)` | `COALESCE(a, b)` | Simple replace |
| `IF(cond, a, b)` | `CASE WHEN cond THEN a ELSE b END` | Regex |
| `GROUP_CONCAT(...)` | `STRING_AGG(... , ',')` | Complex regex |
| `AUTO_INCREMENT` | `GENERATED ALWAYS AS IDENTITY` | Regex |
| `ON DUPLICATE KEY UPDATE` | `ON CONFLICT ... DO UPDATE` | Complex |
| `NOW()` | `NOW()` | Compatible |
| `UNIX_TIMESTAMP()` | `EXTRACT(EPOCH FROM NOW())` | Regex |
| `DATE_FORMAT()` | `TO_CHAR()` | Complex mapping |

### Untranslatable Queries
Queries that can't be transformed are:
- Marked as `QueryKind::Other` with a skip reason
- Logged as warnings
- Counted in the capture report
- Not included in the replay workload

### CaptureSource Trait
Formalize the existing pattern:
```rust
pub trait CaptureSource {
    fn capture(&self, config: &CaptureConfig) -> Result<WorkloadProfile>;
}
```

Both `CsvLogCapture` and MySQL parsers implement this trait.

### New Modules
- `src/capture/mysql_general.rs` — General query log parser
- `src/capture/mysql_slow.rs` — Slow query log parser
- `src/transform/mod.rs` — Transform pipeline
- `src/transform/mysql_to_pg.rs` — MySQL-to-PG transforms

### New Dependencies
- `regex` — For SQL transformation rules
- `sqlparser` (optional) — AST-based transformation for complex cases

### Implementation Tasks
1. CaptureSource trait formalization
2. MySQL slow query log parser
3. MySQL general query log parser
4. Transform pipeline framework
5. MySQL-to-PG transform rules (regex-based)
6. Transform quality report (coverage, skip rate)
7. CLI integration (`pg-retest capture --source-type mysql-slow --source-log ...`)

### Extension Path
Oracle, MariaDB, SQL Server follow the same pattern:
- New parser implementing `CaptureSource`
- New transform pipeline implementing `SqlTransformer`
- Same replay engine (output is always PG-compatible SQL)
