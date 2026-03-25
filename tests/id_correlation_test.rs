use chrono::Utc;
use pg_retest::correlate::sequence::SequenceState;
use pg_retest::profile::io;
use pg_retest::profile::{Metadata, Query, QueryKind, Session, WorkloadProfile};
use tempfile::NamedTempFile;

#[test]
fn test_sequence_snapshot_in_profile() {
    let profile = WorkloadProfile {
        version: 2,
        captured_at: Utc::now(),
        source_host: "localhost".into(),
        pg_version: "16.2".into(),
        capture_method: "csv_log".into(),
        sessions: vec![Session {
            id: 1,
            user: "app".into(),
            database: "mydb".into(),
            queries: vec![Query {
                sql: "SELECT 1".into(),
                start_offset_us: 0,
                duration_us: 100,
                kind: QueryKind::Select,
                transaction_id: None,
                response_values: None,
            }],
        }],
        metadata: Metadata {
            total_queries: 1,
            total_sessions: 1,
            capture_duration_us: 100,
            sequence_snapshot: Some(vec![
                SequenceState {
                    schema: "public".into(),
                    name: "orders_id_seq".into(),
                    last_value: Some(42),
                    increment_by: 1,
                    start_value: 1,
                    min_value: 1,
                    max_value: i64::MAX,
                    cycle: false,
                    is_called: true,
                },
                SequenceState {
                    schema: "public".into(),
                    name: "users_id_seq".into(),
                    last_value: None,
                    increment_by: 1,
                    start_value: 1,
                    min_value: 1,
                    max_value: i64::MAX,
                    cycle: false,
                    is_called: false,
                },
            ]),
            pk_map: None,
        },
    };
    let file = NamedTempFile::new().unwrap();
    io::write_profile(file.path(), &profile).unwrap();
    let loaded = io::read_profile(file.path()).unwrap();
    let snap = loaded.metadata.sequence_snapshot.unwrap();
    assert_eq!(snap.len(), 2);
    assert_eq!(snap[0].last_value, Some(42));
    assert!(snap[0].is_called);
    assert_eq!(snap[1].last_value, None);
}

#[test]
fn test_profile_backward_compat() {
    let profile = WorkloadProfile {
        version: 2,
        captured_at: Utc::now(),
        source_host: "localhost".into(),
        pg_version: "16.2".into(),
        capture_method: "csv_log".into(),
        sessions: vec![Session {
            id: 1,
            user: "app".into(),
            database: "mydb".into(),
            queries: vec![Query {
                sql: "SELECT 1".into(),
                start_offset_us: 0,
                duration_us: 100,
                kind: QueryKind::Select,
                transaction_id: None,
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
    assert!(loaded.metadata.sequence_snapshot.is_none());
    assert!(loaded.sessions[0].queries[0].response_values.is_none());
}
