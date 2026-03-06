use pg_retest::capture::rds::{parse_log_file_list, select_latest_log_file};

#[test]
fn test_parse_log_file_list() {
    let json = r#"{
        "DescribeDBLogFiles": [
            {
                "LogFileName": "error/postgresql.log.2024-03-08-08",
                "LastWritten": 1709884800000,
                "Size": 1048576
            },
            {
                "LogFileName": "error/postgresql.log.2024-03-08-10",
                "LastWritten": 1709892000000,
                "Size": 524288
            },
            {
                "LogFileName": "error/postgresql.log.2024-03-08-09",
                "LastWritten": 1709888400000,
                "Size": 786432
            }
        ]
    }"#;

    let files = parse_log_file_list(json).unwrap();
    assert_eq!(files.len(), 3);
    assert_eq!(files[0].log_file_name, "error/postgresql.log.2024-03-08-08");
}

#[test]
fn test_select_latest_log_file() {
    let json = r#"{
        "DescribeDBLogFiles": [
            {
                "LogFileName": "error/postgresql.log.2024-03-08-08",
                "LastWritten": 1709884800000,
                "Size": 1048576
            },
            {
                "LogFileName": "error/postgresql.log.2024-03-08-10",
                "LastWritten": 1709892000000,
                "Size": 524288
            }
        ]
    }"#;

    let files = parse_log_file_list(json).unwrap();
    let latest = select_latest_log_file(&files).unwrap();
    assert_eq!(latest, "error/postgresql.log.2024-03-08-10");
}

#[test]
fn test_parse_empty_log_file_list() {
    let json = r#"{ "DescribeDBLogFiles": [] }"#;
    let files = parse_log_file_list(json).unwrap();
    assert!(files.is_empty());
    assert!(select_latest_log_file(&files).is_none());
}
