//! inject_returning corpus test (SC-007). Output format per line:
//! `<input>|<output or NONE>`. Regenerate with REGEN_SNAPSHOTS=1.

use pg_retest::correlate::capture::{inject_returning, TablePk};
use std::fs;
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn load_corpus() -> Vec<String> {
    let text =
        fs::read_to_string(fixtures_dir().join("returning_corpus.txt")).expect("corpus file");
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|s| s.to_string())
        .collect()
}

fn pk_map() -> Vec<TablePk> {
    vec![
        TablePk {
            schema: "public".into(),
            table: "orders".into(),
            columns: vec!["id".into()],
        },
        TablePk {
            schema: "analytics".into(),
            table: "events".into(),
            columns: vec!["event_id".into()],
        },
        TablePk {
            schema: "public".into(),
            table: "t".into(),
            columns: vec!["a".into(), "b".into()],
        },
    ]
}

#[test]
fn returning_gold_snapshot() {
    let corpus = load_corpus();
    assert!(
        corpus.len() >= 15,
        "corpus must have at least 15 queries (found {})",
        corpus.len()
    );
    let pk = pk_map();

    let actual: Vec<String> = corpus
        .iter()
        .map(|sql| match inject_returning(sql, &pk) {
            Some(out) => format!("{}|{}", sql, out),
            None => format!("{}|NONE", sql),
        })
        .collect();

    let expected_path = fixtures_dir().join("returning_expected.txt");

    if std::env::var("REGEN_SNAPSHOTS").is_ok() {
        let mut out = String::new();
        for line in &actual {
            out.push_str(line);
            out.push('\n');
        }
        fs::write(&expected_path, &out).expect("write expected");
        eprintln!("regenerated {}", expected_path.display());
        return;
    }

    let expected_text = fs::read_to_string(&expected_path)
        .expect("expected file — generate with REGEN_SNAPSHOTS=1");
    let expected: Vec<&str> = expected_text.lines().collect();

    assert_eq!(actual.len(), expected.len(), "line count mismatch");
    for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        assert_eq!(
            a, e,
            "line {} mismatch\ninput:    {}\nactual:   {}\nexpected: {}",
            i, corpus[i], a, e
        );
    }
}
