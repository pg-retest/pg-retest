//! Map pg-retest's `SourceDialect` to polyglot-sql's `DialectType`.
//!
//! Only compiled with the `polyglot-transform` feature. Spec:
//! `retest-research/plan/include/polyglot-sql-transform.md` FR-XFORM-12.
//! Variant names verified against the 0.5.4 clone (`dialects/mod.rs:195-264`):
//! it is `PostgreSQL` (not `Postgres`) and SQL Server maps to `TSQL`.

use polyglot_sql::DialectType;

use crate::profile::SourceDialect;

/// Resolve a captured workload's origin dialect to the polyglot parse dialect.
pub fn to_polyglot(d: SourceDialect) -> DialectType {
    match d {
        SourceDialect::Postgres => DialectType::PostgreSQL,
        SourceDialect::MySql => DialectType::MySQL,
        SourceDialect::Oracle => DialectType::Oracle,
        SourceDialect::SqlServer => DialectType::TSQL,
        SourceDialect::Snowflake => DialectType::Snowflake,
        SourceDialect::ClickHouse => DialectType::ClickHouse,
        SourceDialect::Other => DialectType::Generic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dialect_mapping_targets_postgresql_not_postgres() {
        // The load-bearing gotcha: the Rust enum variant is `PostgreSQL`.
        assert_eq!(to_polyglot(SourceDialect::Postgres), DialectType::PostgreSQL);
        assert_eq!(to_polyglot(SourceDialect::MySql), DialectType::MySQL);
        assert_eq!(to_polyglot(SourceDialect::SqlServer), DialectType::TSQL);
        assert_eq!(to_polyglot(SourceDialect::Other), DialectType::Generic);
    }
}
