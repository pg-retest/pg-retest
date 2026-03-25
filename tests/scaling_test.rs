use chrono::Utc;
use pg_retest::profile::{Metadata, Query, QueryKind, Session, WorkloadProfile};
use pg_retest::replay::scaling::{check_write_safety, scale_sessions};
use pg_retest::replay::{QueryResult, ReplayResults};

fn make_profile(sessions: Vec<Session>) -> WorkloadProfile {
    let total_queries = sessions.iter().map(|s| s.queries.len() as u64).sum();
    let total_sessions = sessions.len() as u64;
    WorkloadProfile {
        version: 2,
        captured_at: Utc::now(),
        source_host: "test".into(),
        pg_version: "16.2".into(),
        capture_method: "csv_log".into(),
        sessions,
        metadata: Metadata {
            total_queries,
            total_sessions,
            capture_duration_us: 10000,
            sequence_snapshot: None,
            pk_map: None,
        },
    }
}

#[test]
fn test_scale_sessions_1x_returns_original() {
    let profile = make_profile(vec![Session {
        id: 1,
        user: "app".into(),
        database: "db".into(),
        queries: vec![Query {
            sql: "SELECT 1".into(),
            start_offset_us: 0,
            duration_us: 100,
            kind: QueryKind::Select,
            transaction_id: None,
            response_values: None,
        }],
    }]);

    let scaled = scale_sessions(&profile, 1, 0);
    assert_eq!(scaled.len(), 1);
    assert_eq!(scaled[0].id, 1);
}

#[test]
fn test_scale_sessions_3x() {
    let profile = make_profile(vec![
        Session {
            id: 1,
            user: "app".into(),
            database: "db".into(),
            queries: vec![Query {
                sql: "SELECT 1".into(),
                start_offset_us: 0,
                duration_us: 100,
                kind: QueryKind::Select,
                transaction_id: None,
                response_values: None,
            }],
        },
        Session {
            id: 2,
            user: "admin".into(),
            database: "db".into(),
            queries: vec![Query {
                sql: "SELECT 2".into(),
                start_offset_us: 0,
                duration_us: 200,
                kind: QueryKind::Select,
                transaction_id: None,
                response_values: None,
            }],
        },
    ]);

    let scaled = scale_sessions(&profile, 3, 0);
    assert_eq!(scaled.len(), 6); // 2 sessions * 3x

    // Original IDs
    assert_eq!(scaled[0].id, 1);
    assert_eq!(scaled[1].id, 2);
    // Copy 1 IDs: original + 1*session_count
    assert_eq!(scaled[2].id, 3);
    assert_eq!(scaled[3].id, 4);
    // Copy 2 IDs: original + 2*session_count
    assert_eq!(scaled[4].id, 5);
    assert_eq!(scaled[5].id, 6);
}

#[test]
fn test_scale_sessions_stagger() {
    let profile = make_profile(vec![Session {
        id: 1,
        user: "app".into(),
        database: "db".into(),
        queries: vec![
            Query {
                sql: "SELECT 1".into(),
                start_offset_us: 0,
                duration_us: 100,
                kind: QueryKind::Select,
                transaction_id: None,
                response_values: None,
            },
            Query {
                sql: "SELECT 2".into(),
                start_offset_us: 1000,
                duration_us: 100,
                kind: QueryKind::Select,
                transaction_id: None,
                response_values: None,
            },
        ],
    }]);

    let scaled = scale_sessions(&profile, 2, 500); // 500ms stagger

    // Original copy: offsets unchanged
    assert_eq!(scaled[0].queries[0].start_offset_us, 0);
    assert_eq!(scaled[0].queries[1].start_offset_us, 1000);

    // Second copy: offsets += 500ms = 500_000us
    assert_eq!(scaled[1].queries[0].start_offset_us, 500_000);
    assert_eq!(scaled[1].queries[1].start_offset_us, 501_000);
}

#[test]
fn test_check_write_safety_no_writes() {
    let profile = make_profile(vec![Session {
        id: 1,
        user: "app".into(),
        database: "db".into(),
        queries: vec![Query {
            sql: "SELECT 1".into(),
            start_offset_us: 0,
            duration_us: 100,
            kind: QueryKind::Select,
            transaction_id: None,
            response_values: None,
        }],
    }]);

    assert!(check_write_safety(&profile).is_none());
}

#[test]
fn test_check_write_safety_with_writes() {
    let profile = make_profile(vec![Session {
        id: 1,
        user: "app".into(),
        database: "db".into(),
        queries: vec![
            Query {
                sql: "SELECT 1".into(),
                start_offset_us: 0,
                duration_us: 100,
                kind: QueryKind::Select,
                transaction_id: None,
                response_values: None,
            },
            Query {
                sql: "INSERT INTO t VALUES (1)".into(),
                start_offset_us: 100,
                duration_us: 200,
                kind: QueryKind::Insert,
                transaction_id: None,
                response_values: None,
            },
        ],
    }]);

    let warning = check_write_safety(&profile);
    assert!(warning.is_some());
    assert!(warning.unwrap().contains("1 write queries"));
}

#[test]
fn test_scale_report_computation() {
    use pg_retest::compare::capacity::compute_scale_report;

    let results = vec![
        ReplayResults {
            session_id: 1,
            query_results: vec![
                QueryResult {
                    sql: "SELECT 1".into(),
                    original_duration_us: 100,
                    replay_duration_us: 80,
                    success: true,
                    error: None,
                },
                QueryResult {
                    sql: "SELECT 2".into(),
                    original_duration_us: 200,
                    replay_duration_us: 150,
                    success: true,
                    error: None,
                },
            ],
        },
        ReplayResults {
            session_id: 2,
            query_results: vec![QueryResult {
                sql: "SELECT 3".into(),
                original_duration_us: 300,
                replay_duration_us: 400,
                success: false,
                error: Some("timeout".into()),
            }],
        },
    ];

    let report = compute_scale_report(&results, 2, 1_000_000); // 1 second elapsed

    assert_eq!(report.scale_factor, 2);
    assert_eq!(report.total_sessions, 2);
    assert_eq!(report.total_queries, 3);
    assert_eq!(report.error_count, 1);
    assert!((report.throughput_qps - 3.0).abs() < 0.1); // 3 queries in 1 second
    assert!((report.error_rate_pct - 33.33).abs() < 0.1); // 1/3
}
