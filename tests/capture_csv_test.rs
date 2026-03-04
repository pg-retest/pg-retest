use pg_retest::capture::csv_log::CsvLogCapture;
use pg_retest::profile::QueryKind;
use std::path::Path;

#[test]
fn test_csv_log_capture_parses_sessions() {
    let capture = CsvLogCapture;
    let path = Path::new("tests/fixtures/sample_pg.csv");
    let profile = capture
        .capture_from_file(path, "localhost", "16.2")
        .unwrap();

    assert_eq!(profile.version, 2);
    assert_eq!(profile.capture_method, "csv_log");
    assert_eq!(profile.sessions.len(), 2);
    assert_eq!(profile.metadata.total_queries, 5);
    assert_eq!(profile.metadata.total_sessions, 2);
}

#[test]
fn test_csv_log_capture_session_ordering() {
    let capture = CsvLogCapture;
    let path = Path::new("tests/fixtures/sample_pg.csv");
    let profile = capture
        .capture_from_file(path, "localhost", "16.2")
        .unwrap();

    // Find session for process_id 1234 (session_id 6600a000.4d2)
    // It should have 3 queries, ordered by timestamp
    let session = profile
        .sessions
        .iter()
        .find(|s| s.user == "app_user" && s.queries.len() == 3)
        .expect("Should find app_user session with 3 queries");

    assert_eq!(session.queries[0].kind, QueryKind::Select);
    assert_eq!(session.queries[1].kind, QueryKind::Update);
    assert_eq!(session.queries[2].kind, QueryKind::Select);

    // Verify relative timing: queries should have increasing start offsets
    assert_eq!(session.queries[0].start_offset_us, 0);
    assert!(session.queries[1].start_offset_us > 0);
    assert!(session.queries[2].start_offset_us > session.queries[1].start_offset_us);
}

#[test]
fn test_csv_log_capture_duration_parsing() {
    let capture = CsvLogCapture;
    let path = Path::new("tests/fixtures/sample_pg.csv");
    let profile = capture
        .capture_from_file(path, "localhost", "16.2")
        .unwrap();

    let session = profile
        .sessions
        .iter()
        .find(|s| s.user == "app_user" && s.queries.len() == 3)
        .expect("Should find app_user session");

    // First query: duration 0.450 ms = 450 us
    assert_eq!(session.queries[0].duration_us, 450);
    // Second query: duration 1.200 ms = 1200 us
    assert_eq!(session.queries[1].duration_us, 1200);
}

#[test]
fn test_csv_log_capture_admin_session() {
    let capture = CsvLogCapture;
    let path = Path::new("tests/fixtures/sample_pg.csv");
    let profile = capture
        .capture_from_file(path, "localhost", "16.2")
        .unwrap();

    let session = profile
        .sessions
        .iter()
        .find(|s| s.user == "admin")
        .expect("Should find admin session");

    assert_eq!(session.queries.len(), 2);
    assert_eq!(session.database, "mydb");
    assert_eq!(session.queries[0].kind, QueryKind::Select);
    assert_eq!(session.queries[1].kind, QueryKind::Insert);
}

// --- Transaction boundary tests ---

#[test]
fn test_csv_log_capture_transaction_grouping() {
    let capture = CsvLogCapture;
    let path = Path::new("tests/fixtures/sample_pg_txn.csv");
    let profile = capture
        .capture_from_file(path, "localhost", "16.2")
        .unwrap();

    // app_user session: BEGIN, UPDATE, SELECT, COMMIT, SELECT (standalone)
    let session = profile
        .sessions
        .iter()
        .find(|s| s.user == "app_user")
        .expect("Should find app_user session");

    assert_eq!(session.queries.len(), 5);

    // First 4 queries should share a transaction_id
    let txn_id = session.queries[0].transaction_id;
    assert!(txn_id.is_some(), "BEGIN should have a transaction_id");
    assert_eq!(session.queries[0].kind, QueryKind::Begin);
    assert_eq!(session.queries[1].transaction_id, txn_id);
    assert_eq!(session.queries[2].transaction_id, txn_id);
    assert_eq!(session.queries[3].kind, QueryKind::Commit);
    assert_eq!(session.queries[3].transaction_id, txn_id);

    // Last query is outside a transaction
    assert_eq!(session.queries[4].transaction_id, None);
}

#[test]
fn test_csv_log_capture_rollback_transaction() {
    let capture = CsvLogCapture;
    let path = Path::new("tests/fixtures/sample_pg_txn.csv");
    let profile = capture
        .capture_from_file(path, "localhost", "16.2")
        .unwrap();

    // admin session: BEGIN, INSERT, ROLLBACK
    let session = profile
        .sessions
        .iter()
        .find(|s| s.user == "admin")
        .expect("Should find admin session");

    assert_eq!(session.queries.len(), 3);
    assert_eq!(session.queries[0].kind, QueryKind::Begin);
    assert_eq!(session.queries[2].kind, QueryKind::Rollback);

    // All should share a transaction_id
    let txn_id = session.queries[0].transaction_id;
    assert!(txn_id.is_some());
    assert_eq!(session.queries[1].transaction_id, txn_id);
    assert_eq!(session.queries[2].transaction_id, txn_id);
}

#[test]
fn test_csv_log_capture_txn_ids_are_unique() {
    let capture = CsvLogCapture;
    let path = Path::new("tests/fixtures/sample_pg_txn.csv");
    let profile = capture
        .capture_from_file(path, "localhost", "16.2")
        .unwrap();

    // Each transaction block should have a distinct ID
    let mut txn_ids: Vec<u64> = Vec::new();
    for session in &profile.sessions {
        for query in &session.queries {
            if query.kind == QueryKind::Begin {
                if let Some(id) = query.transaction_id {
                    txn_ids.push(id);
                }
            }
        }
    }
    assert_eq!(txn_ids.len(), 2); // two BEGIN statements
    assert_ne!(txn_ids[0], txn_ids[1]); // different IDs
}

#[test]
fn test_csv_log_original_fixture_no_transactions() {
    let capture = CsvLogCapture;
    let path = Path::new("tests/fixtures/sample_pg.csv");
    let profile = capture
        .capture_from_file(path, "localhost", "16.2")
        .unwrap();

    // Original fixture has no BEGIN/COMMIT, so all transaction_ids should be None
    for session in &profile.sessions {
        for query in &session.queries {
            assert_eq!(
                query.transaction_id, None,
                "Query '{}' should have no transaction_id",
                query.sql
            );
        }
    }
}
