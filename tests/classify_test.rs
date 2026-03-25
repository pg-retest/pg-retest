use chrono::Utc;
use pg_retest::classify::{classify_session, classify_workload, WorkloadClass};
use pg_retest::profile::{Metadata, Query, QueryKind, Session, WorkloadProfile};

fn make_session(id: u64, queries: Vec<Query>) -> Session {
    Session {
        id,
        user: "app".into(),
        database: "db".into(),
        queries,
    }
}

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
fn test_classify_analytical_session() {
    // >80% reads, avg latency >10ms
    let session = make_session(
        1,
        vec![
            Query {
                sql: "SELECT * FROM large_table".into(),
                start_offset_us: 0,
                duration_us: 50_000, // 50ms
                kind: QueryKind::Select,
                transaction_id: None,
                response_values: None,
            },
            Query {
                sql: "SELECT count(*) FROM orders".into(),
                start_offset_us: 1000,
                duration_us: 30_000, // 30ms
                kind: QueryKind::Select,
                transaction_id: None,
                response_values: None,
            },
            Query {
                sql: "SELECT sum(total) FROM orders GROUP BY region".into(),
                start_offset_us: 2000,
                duration_us: 100_000, // 100ms
                kind: QueryKind::Select,
                transaction_id: None,
                response_values: None,
            },
        ],
    );

    let classification = classify_session(&session);
    assert_eq!(classification.class, WorkloadClass::Analytical);
    assert!(classification.read_pct > 80.0);
    assert!(classification.avg_latency_us > 10_000);
}

#[test]
fn test_classify_transactional_session() {
    // >20% writes, avg <5ms, >2 transactions
    let session = make_session(
        1,
        vec![
            Query {
                sql: "BEGIN".into(),
                start_offset_us: 0,
                duration_us: 10,
                kind: QueryKind::Begin,
                transaction_id: Some(1),
                response_values: None,
            },
            Query {
                sql: "SELECT balance FROM accounts".into(),
                start_offset_us: 100,
                duration_us: 500,
                kind: QueryKind::Select,
                transaction_id: Some(1),
                response_values: None,
            },
            Query {
                sql: "UPDATE accounts SET balance = 100".into(),
                start_offset_us: 200,
                duration_us: 800,
                kind: QueryKind::Update,
                transaction_id: Some(1),
                response_values: None,
            },
            Query {
                sql: "COMMIT".into(),
                start_offset_us: 300,
                duration_us: 20,
                kind: QueryKind::Commit,
                transaction_id: Some(1),
                response_values: None,
            },
            Query {
                sql: "BEGIN".into(),
                start_offset_us: 400,
                duration_us: 10,
                kind: QueryKind::Begin,
                transaction_id: Some(2),
                response_values: None,
            },
            Query {
                sql: "INSERT INTO log VALUES (1)".into(),
                start_offset_us: 500,
                duration_us: 600,
                kind: QueryKind::Insert,
                transaction_id: Some(2),
                response_values: None,
            },
            Query {
                sql: "COMMIT".into(),
                start_offset_us: 600,
                duration_us: 20,
                kind: QueryKind::Commit,
                transaction_id: Some(2),
                response_values: None,
            },
            Query {
                sql: "BEGIN".into(),
                start_offset_us: 700,
                duration_us: 10,
                kind: QueryKind::Begin,
                transaction_id: Some(3),
                response_values: None,
            },
            Query {
                sql: "UPDATE accounts SET balance = 200".into(),
                start_offset_us: 800,
                duration_us: 900,
                kind: QueryKind::Update,
                transaction_id: Some(3),
                response_values: None,
            },
            Query {
                sql: "COMMIT".into(),
                start_offset_us: 900,
                duration_us: 20,
                kind: QueryKind::Commit,
                transaction_id: Some(3),
                response_values: None,
            },
        ],
    );

    let classification = classify_session(&session);
    assert_eq!(classification.class, WorkloadClass::Transactional);
    assert!(classification.transaction_count > 2);
}

#[test]
fn test_classify_bulk_session() {
    // >80% writes, <=2 transactions
    let session = make_session(
        1,
        vec![
            Query {
                sql: "INSERT INTO t VALUES (1)".into(),
                start_offset_us: 0,
                duration_us: 100,
                kind: QueryKind::Insert,
                transaction_id: None,
                response_values: None,
            },
            Query {
                sql: "INSERT INTO t VALUES (2)".into(),
                start_offset_us: 100,
                duration_us: 100,
                kind: QueryKind::Insert,
                transaction_id: None,
                response_values: None,
            },
            Query {
                sql: "INSERT INTO t VALUES (3)".into(),
                start_offset_us: 200,
                duration_us: 100,
                kind: QueryKind::Insert,
                transaction_id: None,
                response_values: None,
            },
            Query {
                sql: "INSERT INTO t VALUES (4)".into(),
                start_offset_us: 300,
                duration_us: 100,
                kind: QueryKind::Insert,
                transaction_id: None,
                response_values: None,
            },
            Query {
                sql: "INSERT INTO t VALUES (5)".into(),
                start_offset_us: 400,
                duration_us: 100,
                kind: QueryKind::Insert,
                transaction_id: None,
                response_values: None,
            },
        ],
    );

    let classification = classify_session(&session);
    assert_eq!(classification.class, WorkloadClass::Bulk);
    assert!(classification.write_pct > 80.0);
}

#[test]
fn test_classify_mixed_session() {
    // Balanced reads/writes
    let session = make_session(
        1,
        vec![
            Query {
                sql: "SELECT 1".into(),
                start_offset_us: 0,
                duration_us: 1000,
                kind: QueryKind::Select,
                transaction_id: None,
                response_values: None,
            },
            Query {
                sql: "INSERT INTO t VALUES (1)".into(),
                start_offset_us: 100,
                duration_us: 1000,
                kind: QueryKind::Insert,
                transaction_id: None,
                response_values: None,
            },
        ],
    );

    let classification = classify_session(&session);
    assert_eq!(classification.class, WorkloadClass::Mixed);
}

#[test]
fn test_classify_workload_majority_vote() {
    let profile = make_profile(vec![
        // 3 analytical sessions
        make_session(
            1,
            vec![Query {
                sql: "SELECT * FROM huge".into(),
                start_offset_us: 0,
                duration_us: 50_000,
                kind: QueryKind::Select,
                transaction_id: None,
                response_values: None,
            }],
        ),
        make_session(
            2,
            vec![Query {
                sql: "SELECT sum(x) FROM big".into(),
                start_offset_us: 0,
                duration_us: 40_000,
                kind: QueryKind::Select,
                transaction_id: None,
                response_values: None,
            }],
        ),
        make_session(
            3,
            vec![Query {
                sql: "SELECT avg(y) FROM large".into(),
                start_offset_us: 0,
                duration_us: 60_000,
                kind: QueryKind::Select,
                transaction_id: None,
                response_values: None,
            }],
        ),
        // 1 mixed session
        make_session(
            4,
            vec![
                Query {
                    sql: "SELECT 1".into(),
                    start_offset_us: 0,
                    duration_us: 500,
                    kind: QueryKind::Select,
                    transaction_id: None,
                    response_values: None,
                },
                Query {
                    sql: "INSERT INTO t VALUES (1)".into(),
                    start_offset_us: 100,
                    duration_us: 500,
                    kind: QueryKind::Insert,
                    transaction_id: None,
                    response_values: None,
                },
            ],
        ),
    ]);

    let wc = classify_workload(&profile);
    assert_eq!(wc.overall_class, WorkloadClass::Analytical);
    assert_eq!(wc.class_counts.analytical, 3);
    assert_eq!(wc.class_counts.mixed, 1);
}

#[test]
fn test_classify_empty_session() {
    let session = make_session(1, vec![]);
    let classification = classify_session(&session);
    assert_eq!(classification.class, WorkloadClass::Mixed);
    assert_eq!(classification.query_count, 0);
}
