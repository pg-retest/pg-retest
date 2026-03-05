use pg_retest::capture::mysql_slow::MysqlSlowLogCapture;
use pg_retest::profile::io;
use tempfile::NamedTempFile;

#[test]
fn test_mysql_capture_roundtrip() {
    // Capture from MySQL slow log with transform
    let capture = MysqlSlowLogCapture;
    let profile = capture
        .capture_from_file(
            std::path::Path::new("tests/fixtures/sample_mysql_slow.log"),
            "mysql-test",
            true,
        )
        .unwrap();

    // Write to .wkl file
    let file = NamedTempFile::with_suffix(".wkl").unwrap();
    io::write_profile(file.path(), &profile).unwrap();

    // Read back
    let loaded = io::read_profile(file.path()).unwrap();

    assert_eq!(loaded.source_host, "mysql-test");
    assert_eq!(loaded.capture_method, "mysql_slow_log");
    assert_eq!(
        loaded.metadata.total_sessions,
        profile.metadata.total_sessions
    );
    assert_eq!(
        loaded.metadata.total_queries,
        profile.metadata.total_queries
    );

    // Verify no backticks in any SQL (transform applied)
    for session in &loaded.sessions {
        for query in &session.queries {
            assert!(
                !query.sql.contains('`'),
                "Found backtick in SQL after transform: {}",
                query.sql
            );
        }
    }
}

#[test]
fn test_mysql_capture_with_masking() {
    use pg_retest::capture::masking::mask_sql_literals;

    let capture = MysqlSlowLogCapture;
    let mut profile = capture
        .capture_from_file(
            std::path::Path::new("tests/fixtures/sample_mysql_slow.log"),
            "test",
            true,
        )
        .unwrap();

    // Apply masking
    for session in &mut profile.sessions {
        for query in &mut session.queries {
            query.sql = mask_sql_literals(&query.sql);
        }
    }

    // Verify masking was applied — numeric values should be replaced with $N
    let has_masked = profile
        .sessions
        .iter()
        .flat_map(|s| &s.queries)
        .any(|q| q.sql.contains("$N") || q.sql.contains("$S"));
    assert!(
        has_masked,
        "PII masking should have replaced at least some literals"
    );
}

#[test]
fn test_mysql_pipeline_config_roundtrip() {
    use pg_retest::config::{CaptureConfig, PipelineConfig, ReplayConfig};
    use pg_retest::pipeline;
    use std::path::PathBuf;

    let config = PipelineConfig {
        capture: Some(CaptureConfig {
            workload: None,
            source_log: Some(PathBuf::from("tests/fixtures/sample_mysql_slow.log")),
            source_host: Some("mysql-test".into()),
            pg_version: None,
            source_type: "mysql-slow".into(),
            mask_values: false,
        }),
        provision: None,
        replay: ReplayConfig {
            speed: 0.0,
            read_only: true,
            scale: 1,
            stagger_ms: 0,
            target: Some("host=127.0.0.1 port=1 dbname=test".into()), // will fail at replay
        },
        thresholds: None,
        output: None,
    };

    let result = pipeline::run_pipeline(&config);
    // Pipeline should get past capture (not EXIT_CAPTURE_ERROR)
    // It will fail at replay since port 1 is not reachable, but run_replay
    // absorbs per-session connection errors, so the pipeline completes.
    assert_ne!(
        result.exit_code,
        pipeline::EXIT_CAPTURE_ERROR,
        "Pipeline should not fail at capture stage"
    );
}
