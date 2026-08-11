//! End-to-end integration: MySQL slow-log capture → polyglot transform pipeline.
//! Only built with the experimental `polyglot-transform` feature.
//!
//!   cargo test --features polyglot-transform --test polyglot_integration_test
#![cfg(feature = "polyglot-transform")]

use std::path::Path;

use pg_retest::capture::mysql_slow::MysqlSlowLogCapture;
use pg_retest::profile::SourceDialect;
use pg_retest::transform::is_valid_postgres;
use pg_retest::transform::polyglot::mysql_to_pg_polyglot_pipeline;
use pg_retest::transform::{TransformReport, TransformResult};

#[test]
fn test_capture_then_polyglot_transform_end_to_end() {
    // 1. Capture a real MySQL slow log WITHOUT transform → raw MySQL workload.
    let profile = MysqlSlowLogCapture
        .capture_from_file(
            Path::new("tests/fixtures/sample_mysql_slow.log"),
            "mysql-prod",
            false, // no regex transform — we want raw MySQL to feed the transpiler
        )
        .expect("capture should succeed");

    // P0: the capture path stamped the origin dialect.
    assert_eq!(profile.source_dialect, SourceDialect::MySql);
    assert_eq!(profile.capture_method, "mysql_slow_log");
    let total_captured: usize = profile.sessions.iter().map(|s| s.queries.len()).sum();
    assert!(total_captured >= 6, "captured {total_captured} queries");

    // 2. Run the whole captured workload through the polyglot transform pipeline.
    let pipeline = mysql_to_pg_polyglot_pipeline();
    let mut report = TransformReport::default();
    let mut transformed = Vec::new();
    for session in &profile.sessions {
        for q in &session.queries {
            let result = pipeline.apply(&q.sql);
            report.record(&q.sql, &result);
            if let TransformResult::Transformed(out) = &result {
                transformed.push(out.clone());
            }
        }
    }

    // 3. At least the backtick + `LIMIT 10, 20` query must have been rewritten.
    assert!(
        report.transformed >= 1,
        "expected >=1 transformed query, report: total={} transformed={} unchanged={} skipped={}",
        report.total_queries,
        report.transformed,
        report.unchanged,
        report.skipped
    );

    // 4. Honesty guarantee end-to-end: every emitted Transformed output is valid PG.
    for out in &transformed {
        assert!(
            is_valid_postgres(out),
            "transformed output not valid PG: {out}"
        );
        assert!(!out.contains('`'), "backticks must be gone: {out}");
    }

    // 5. The backtick + LIMIT-offset query is rewritten with double quotes + OFFSET.
    assert!(
        transformed
            .iter()
            .any(|o| o.contains('"') && o.to_uppercase().contains("OFFSET")),
        "expected the backtick+LIMIT query rewritten with double-quotes and OFFSET; got: {transformed:?}"
    );
}
