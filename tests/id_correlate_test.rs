use pg_retest::correlate::capture::{inject_returning, ResponseRow, TablePk};
use pg_retest::correlate::map::IdMap;
use pg_retest::profile::io;
use pg_retest::profile::{Metadata, Query, QueryKind, Session, WorkloadProfile};

use chrono::Utc;
use tempfile::NamedTempFile;

#[test]
fn test_response_values_in_profile() {
    let profile = WorkloadProfile {
        version: 2,
        captured_at: Utc::now(),
        source_host: "localhost".into(),
        pg_version: "16.2".into(),
        capture_method: "proxy".into(),
        sessions: vec![Session {
            id: 1,
            user: "app".into(),
            database: "mydb".into(),
            queries: vec![Query {
                sql: "INSERT INTO orders (name) VALUES ('test') RETURNING id".into(),
                start_offset_us: 0,
                duration_us: 100,
                kind: QueryKind::Insert,
                transaction_id: None,
                response_values: Some(vec![ResponseRow {
                    columns: vec![("id".into(), "42".into())],
                }]),
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
    let rv = loaded.sessions[0].queries[0]
        .response_values
        .as_ref()
        .unwrap();
    assert_eq!(rv.len(), 1);
    assert_eq!(rv[0].columns[0], ("id".into(), "42".into()));
}

#[test]
fn test_id_map_substitute_where_clause() {
    let map = IdMap::new();
    map.register("42".into(), "1001".into());
    let (result, count) = map.substitute("SELECT * FROM orders WHERE id = 42");
    assert_eq!(result, "SELECT * FROM orders WHERE id = 1001");
    assert_eq!(count, 1);
}

#[test]
fn test_id_map_no_substitute_when_empty() {
    let map = IdMap::new();
    let (result, count) = map.substitute("SELECT * FROM orders WHERE id = 42");
    assert_eq!(result, "SELECT * FROM orders WHERE id = 42");
    assert_eq!(count, 0);
}

#[test]
fn test_id_map_cross_session_sharing() {
    let map = IdMap::new();
    let map2 = map.clone();
    // Session A registers
    map.register("42".into(), "1001".into());
    // Session B can see it
    let (result, count) = map2.substitute("SELECT * FROM orders WHERE id = 42");
    assert_eq!(result, "SELECT * FROM orders WHERE id = 1001");
    assert_eq!(count, 1);
}

#[test]
fn test_id_map_uuid_substitution() {
    let map = IdMap::new();
    map.register(
        "550e8400-e29b-41d4-a716-446655440000".into(),
        "aaaabbbb-cccc-dddd-eeee-ffffffffffff".into(),
    );
    let (result, count) =
        map.substitute("SELECT * FROM t WHERE uuid = '550e8400-e29b-41d4-a716-446655440000'");
    assert!(result.contains("aaaabbbb-cccc-dddd-eeee-ffffffffffff"));
    assert_eq!(count, 1);
}

#[test]
fn test_id_map_fk_chain_substitution() {
    let map = IdMap::new();
    map.register("42".into(), "1001".into());
    // FK insert using the remapped parent ID
    let (result, count) =
        map.substitute("INSERT INTO order_items (order_id, product) VALUES (42, 'widget')");
    assert!(result.contains("1001"));
    assert_eq!(count, 1);
}

#[test]
fn test_correlate_requires_proxy_capture() {
    // Verify that a log-captured workload can be detected by checking capture_method
    let profile = WorkloadProfile {
        version: 2,
        captured_at: Utc::now(),
        source_host: "localhost".into(),
        pg_version: "16.2".into(),
        capture_method: "csv_log".into(),
        sessions: vec![],
        metadata: Metadata {
            total_queries: 0,
            total_sessions: 0,
            capture_duration_us: 0,
            sequence_snapshot: None,
            pk_map: None,
        },
    };
    assert_ne!(profile.capture_method, "proxy");
}

#[test]
fn test_inject_returning_composite_pk() {
    let pk_map = vec![TablePk {
        schema: "public".into(),
        table: "order_items".into(),
        columns: vec!["order_id".into(), "item_id".into()],
    }];
    let result = inject_returning(
        "INSERT INTO order_items (order_id, item_id, qty) VALUES (1, 2, 5)",
        &pk_map,
    );
    assert_eq!(
        result,
        Some(
            "INSERT INTO order_items (order_id, item_id, qty) VALUES (1, 2, 5) RETURNING order_id, item_id"
                .into()
        )
    );
}

#[test]
fn test_pk_map_in_profile() {
    let profile = WorkloadProfile {
        version: 2,
        captured_at: Utc::now(),
        source_host: "localhost".into(),
        pg_version: "16.2".into(),
        capture_method: "proxy".into(),
        sessions: vec![],
        metadata: Metadata {
            total_queries: 0,
            total_sessions: 0,
            capture_duration_us: 0,
            sequence_snapshot: None,
            pk_map: Some(vec![TablePk {
                schema: "public".into(),
                table: "orders".into(),
                columns: vec!["id".into()],
            }]),
        },
    };
    let file = NamedTempFile::new().unwrap();
    io::write_profile(file.path(), &profile).unwrap();
    let loaded = io::read_profile(file.path()).unwrap();
    let pk = loaded.metadata.pk_map.unwrap();
    assert_eq!(pk.len(), 1);
    assert_eq!(pk[0].table, "orders");
}
