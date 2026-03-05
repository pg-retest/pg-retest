use pg_retest::compare::junit::write_junit_xml;
use pg_retest::compare::threshold::ThresholdResult;
use tempfile::NamedTempFile;

#[test]
fn test_junit_xml_all_pass() {
    let results = vec![
        ThresholdResult {
            name: "p95_latency".into(),
            passed: true,
            actual: 45.0,
            limit: 50.0,
            message: None,
        },
        ThresholdResult {
            name: "error_rate".into(),
            passed: true,
            actual: 0.5,
            limit: 1.0,
            message: None,
        },
    ];

    let file = NamedTempFile::new().unwrap();
    write_junit_xml(file.path(), &results, 1.5).unwrap();

    let content = std::fs::read_to_string(file.path()).unwrap();
    assert!(content.contains("tests=\"2\" failures=\"0\""));
    assert!(content.contains("name=\"p95_latency\""));
    assert!(content.contains("name=\"error_rate\""));
    assert!(!content.contains("<failure"));
}

#[test]
fn test_junit_xml_with_failures() {
    let results = vec![
        ThresholdResult {
            name: "p95_latency".into(),
            passed: true,
            actual: 45.0,
            limit: 50.0,
            message: None,
        },
        ThresholdResult {
            name: "regression_count".into(),
            passed: false,
            actual: 7.0,
            limit: 5.0,
            message: Some("7 regressions found, max allowed: 5".into()),
        },
    ];

    let file = NamedTempFile::new().unwrap();
    write_junit_xml(file.path(), &results, 2.0).unwrap();

    let content = std::fs::read_to_string(file.path()).unwrap();
    assert!(content.contains("tests=\"2\" failures=\"1\""));
    assert!(content.contains("<failure message=\"7 regressions found, max allowed: 5\"/>"));
}

#[test]
fn test_junit_xml_escapes_special_chars() {
    let results = vec![ThresholdResult {
        name: "test_with_<special>&chars".into(),
        passed: false,
        actual: 10.0,
        limit: 5.0,
        message: Some("value > limit & that's bad".into()),
    }];

    let file = NamedTempFile::new().unwrap();
    write_junit_xml(file.path(), &results, 0.1).unwrap();

    let content = std::fs::read_to_string(file.path()).unwrap();
    assert!(content.contains("&amp;"));
    assert!(content.contains("&lt;"));
    assert!(content.contains("&gt;"));
}

#[test]
fn test_junit_xml_empty_results() {
    let file = NamedTempFile::new().unwrap();
    write_junit_xml(file.path(), &[], 0.0).unwrap();

    let content = std::fs::read_to_string(file.path()).unwrap();
    assert!(content.contains("tests=\"0\" failures=\"0\""));
}
