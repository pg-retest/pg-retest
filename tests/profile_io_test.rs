use chrono::Utc;
use pg_retest::profile::io;
use pg_retest::profile::{Metadata, Query, QueryKind, Session, WorkloadProfile};
use tempfile::NamedTempFile;

#[test]
fn test_profile_roundtrip_messagepack() {
    let profile = WorkloadProfile {
        version: 1,
        captured_at: Utc::now(),
        source_host: "localhost".into(),
        pg_version: "16.2".into(),
        capture_method: "csv_log".into(),
        sessions: vec![
            Session {
                id: 1,
                user: "app_user".into(),
                database: "mydb".into(),
                queries: vec![
                    Query {
                        sql: "SELECT 1".into(),
                        start_offset_us: 0,
                        duration_us: 500,
                        kind: QueryKind::Select,
                        transaction_id: None,
                        response_values: None,
                    },
                    Query {
                        sql: "UPDATE users SET name = 'test' WHERE id = 1".into(),
                        start_offset_us: 1000,
                        duration_us: 1200,
                        kind: QueryKind::Update,
                        transaction_id: None,
                        response_values: None,
                    },
                ],
            },
            Session {
                id: 2,
                user: "admin".into(),
                database: "mydb".into(),
                queries: vec![Query {
                    sql: "SELECT count(*) FROM orders".into(),
                    start_offset_us: 200,
                    duration_us: 3000,
                    kind: QueryKind::Select,
                    transaction_id: None,
                    response_values: None,
                }],
            },
        ],
        metadata: Metadata {
            total_queries: 3,
            total_sessions: 2,
            capture_duration_us: 5000,
            sequence_snapshot: None,
            pk_map: None,
        },
    };

    let file = NamedTempFile::new().unwrap();
    let path = file.path();

    io::write_profile(path, &profile).unwrap();
    let loaded = io::read_profile(path).unwrap();

    assert_eq!(loaded.version, 1);
    assert_eq!(loaded.source_host, "localhost");
    assert_eq!(loaded.pg_version, "16.2");
    assert_eq!(loaded.capture_method, "csv_log");
    assert_eq!(loaded.sessions.len(), 2);
    assert_eq!(loaded.sessions[0].queries.len(), 2);
    assert_eq!(loaded.sessions[0].queries[0].sql, "SELECT 1");
    assert_eq!(loaded.sessions[0].queries[0].kind, QueryKind::Select);
    assert_eq!(loaded.sessions[0].queries[1].kind, QueryKind::Update);
    assert_eq!(loaded.sessions[1].queries[0].duration_us, 3000);
    assert_eq!(loaded.metadata.total_queries, 3);
}

#[test]
fn test_query_kind_classification() {
    assert_eq!(
        QueryKind::from_sql("SELECT * FROM users"),
        QueryKind::Select
    );
    assert_eq!(
        QueryKind::from_sql("select count(*) from orders"),
        QueryKind::Select
    );
    assert_eq!(
        QueryKind::from_sql("INSERT INTO users VALUES (1)"),
        QueryKind::Insert
    );
    assert_eq!(
        QueryKind::from_sql("UPDATE users SET x=1"),
        QueryKind::Update
    );
    assert_eq!(
        QueryKind::from_sql("DELETE FROM users WHERE id=1"),
        QueryKind::Delete
    );
    assert_eq!(
        QueryKind::from_sql("CREATE TABLE foo (id int)"),
        QueryKind::Ddl
    );
    assert_eq!(
        QueryKind::from_sql("ALTER TABLE foo ADD COLUMN bar text"),
        QueryKind::Ddl
    );
    assert_eq!(QueryKind::from_sql("DROP TABLE foo"), QueryKind::Ddl);
    assert_eq!(QueryKind::from_sql("VACUUM users"), QueryKind::Other);
    assert_eq!(QueryKind::from_sql("BEGIN"), QueryKind::Begin);
    assert_eq!(QueryKind::from_sql("COMMIT"), QueryKind::Commit);
}

// --- Transaction-related classification tests ---

#[test]
fn test_query_kind_transaction_control() {
    assert_eq!(QueryKind::from_sql("BEGIN"), QueryKind::Begin);
    assert_eq!(QueryKind::from_sql("START TRANSACTION"), QueryKind::Begin);
    assert_eq!(QueryKind::from_sql("COMMIT"), QueryKind::Commit);
    assert_eq!(QueryKind::from_sql("END"), QueryKind::Commit);
    assert_eq!(QueryKind::from_sql("ROLLBACK"), QueryKind::Rollback);
    assert_eq!(QueryKind::from_sql("ABORT"), QueryKind::Rollback);
}

#[test]
fn test_is_transaction_control() {
    assert!(QueryKind::Begin.is_transaction_control());
    assert!(QueryKind::Commit.is_transaction_control());
    assert!(QueryKind::Rollback.is_transaction_control());
    assert!(!QueryKind::Select.is_transaction_control());
    assert!(!QueryKind::Insert.is_transaction_control());
    assert!(!QueryKind::Other.is_transaction_control());
}

#[test]
fn test_transaction_control_not_read_only() {
    assert!(!QueryKind::Begin.is_read_only());
    assert!(!QueryKind::Commit.is_read_only());
    assert!(!QueryKind::Rollback.is_read_only());
}

#[test]
fn test_profile_roundtrip_with_transaction_id() {
    let profile = WorkloadProfile {
        version: 2,
        captured_at: Utc::now(),
        source_host: "localhost".into(),
        pg_version: "16.2".into(),
        capture_method: "csv_log".into(),
        sessions: vec![Session {
            id: 1,
            user: "app".into(),
            database: "db".into(),
            queries: vec![
                Query {
                    sql: "BEGIN".into(),
                    start_offset_us: 0,
                    duration_us: 10,
                    kind: QueryKind::Begin,
                    transaction_id: Some(1),
                    response_values: None,
                },
                Query {
                    sql: "UPDATE t SET x=1".into(),
                    start_offset_us: 100,
                    duration_us: 500,
                    kind: QueryKind::Update,
                    transaction_id: Some(1),
                    response_values: None,
                },
                Query {
                    sql: "COMMIT".into(),
                    start_offset_us: 200,
                    duration_us: 20,
                    kind: QueryKind::Commit,
                    transaction_id: Some(1),
                    response_values: None,
                },
                Query {
                    sql: "SELECT 1".into(),
                    start_offset_us: 300,
                    duration_us: 100,
                    kind: QueryKind::Select,
                    transaction_id: None,
                    response_values: None,
                },
            ],
        }],
        metadata: Metadata {
            total_queries: 4,
            total_sessions: 1,
            capture_duration_us: 300,
            sequence_snapshot: None,
            pk_map: None,
        },
    };

    let file = NamedTempFile::new().unwrap();
    io::write_profile(file.path(), &profile).unwrap();
    let loaded = io::read_profile(file.path()).unwrap();

    assert_eq!(loaded.version, 2);
    assert_eq!(loaded.sessions[0].queries[0].transaction_id, Some(1));
    assert_eq!(loaded.sessions[0].queries[1].transaction_id, Some(1));
    assert_eq!(loaded.sessions[0].queries[2].transaction_id, Some(1));
    assert_eq!(loaded.sessions[0].queries[3].transaction_id, None);
}

#[test]
fn test_v1_profile_deserializes_with_none_transaction_id() {
    // v1 profile (no transaction_id) should deserialize with None thanks to #[serde(default)]
    let profile = WorkloadProfile {
        version: 1,
        captured_at: Utc::now(),
        source_host: "old-host".into(),
        pg_version: "15.0".into(),
        capture_method: "csv_log".into(),
        sessions: vec![Session {
            id: 1,
            user: "app".into(),
            database: "db".into(),
            queries: vec![Query {
                sql: "SELECT 1".into(),
                start_offset_us: 0,
                duration_us: 100,
                kind: QueryKind::Select,
                transaction_id: None, // simulates a v1 profile
                response_values: None,
            }],
        }],
        metadata: Metadata {
            total_queries: 1,
            total_sessions: 1,
            capture_duration_us: 100,
            sequence_snapshot: None,
            pk_map: None,
        },
    };

    let file = NamedTempFile::new().unwrap();
    io::write_profile(file.path(), &profile).unwrap();
    let loaded = io::read_profile(file.path()).unwrap();

    assert_eq!(loaded.version, 1);
    assert_eq!(loaded.sessions[0].queries[0].transaction_id, None);
}

#[test]
fn test_rollback_to_savepoint_not_classified_as_rollback() {
    // ROLLBACK TO SAVEPOINT should be Other, not Rollback
    assert_eq!(QueryKind::from_sql("ROLLBACK TO sp1"), QueryKind::Other);
    assert_eq!(
        QueryKind::from_sql("ROLLBACK TO SAVEPOINT sp1"),
        QueryKind::Other
    );
    assert_eq!(
        QueryKind::from_sql("  rollback to savepoint my_sp"),
        QueryKind::Other
    );
    // Plain ROLLBACK should still be Rollback
    assert_eq!(QueryKind::from_sql("ROLLBACK"), QueryKind::Rollback);
}

#[test]
fn test_transaction_ids_preserved_through_savepoint_rollback() {
    use pg_retest::profile::assign_transaction_ids;

    let mut queries = vec![
        Query {
            sql: "BEGIN".into(),
            start_offset_us: 0,
            duration_us: 10,
            kind: QueryKind::Begin,
            transaction_id: None,
            response_values: None,
        },
        Query {
            sql: "INSERT INTO t VALUES (1)".into(),
            start_offset_us: 100,
            duration_us: 50,
            kind: QueryKind::Insert,
            transaction_id: None,
            response_values: None,
        },
        Query {
            sql: "SAVEPOINT sp1".into(),
            start_offset_us: 200,
            duration_us: 10,
            kind: QueryKind::Other,
            transaction_id: None,
            response_values: None,
        },
        Query {
            sql: "INSERT INTO t VALUES (2)".into(),
            start_offset_us: 300,
            duration_us: 50,
            kind: QueryKind::Insert,
            transaction_id: None,
            response_values: None,
        },
        Query {
            sql: "ROLLBACK TO sp1".into(),
            start_offset_us: 400,
            duration_us: 10,
            kind: QueryKind::Other, // NOT Rollback
            transaction_id: None,
            response_values: None,
        },
        Query {
            sql: "INSERT INTO t VALUES (3)".into(),
            start_offset_us: 500,
            duration_us: 50,
            kind: QueryKind::Insert,
            transaction_id: None,
            response_values: None,
        },
        Query {
            sql: "COMMIT".into(),
            start_offset_us: 600,
            duration_us: 10,
            kind: QueryKind::Commit,
            transaction_id: None,
            response_values: None,
        },
    ];

    let mut next_id = 1;
    assign_transaction_ids(&mut queries, &mut next_id);

    // All queries should be in transaction 1
    for (i, q) in queries.iter().enumerate() {
        assert_eq!(
            q.transaction_id,
            Some(1),
            "Query {} ({}) should have transaction_id=1",
            i,
            q.sql
        );
    }
    assert_eq!(next_id, 2);
}
