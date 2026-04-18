//! Gold-snapshot test: asserts mask_sql_literals produces byte-identical
//! output to a committed fixture. Regenerate the fixture when intentionally
//! changing mask behavior:
//!
//!     REGEN_SNAPSHOTS=1 cargo test --test sql_lexer_mask_snapshot
//!
//! Part of SC-002.
//!
//! # Known divergences from pre-migration impl (documented bug fixes)
//!
//! When `mask_sql_literals` was rewritten on `SqlLexer` (commit introducing
//! this test), the new impl diverges from the hand-rolled pre-migration
//! behavior in two places. Both are bug fixes, not regressions:
//!
//! 1. **Tagged dollar quotes** — `$tag$body$tag$` is now masked as a single
//!    `$S`. The old impl only recognized `$$`, so for `$tag$` it would mask
//!    the *inner* content as `$S` but leave the outer `$tag$...$tag$`
//!    delimiters intact — a PII leak (an observer could see that a quoted
//!    string existed and where it was in the query).
//!
//! 2. **Negative numbers after keyword context** — `SELECT -5` is now
//!    lexed/masked as `SELECT $N`. The old impl required the previous
//!    non-whitespace character to be an operator-class punct and treated
//!    `-` as a bare Punct after keywords, so `SELECT -5` was previously
//!    masked as `SELECT -$N`. Cosmetic; no security impact.
//!
//! Both fixes are captured in the regenerated expected.txt fixture. Keep
//! future regenerations intentional.

use pg_retest::capture::masking::mask_sql_literals;
use std::fs;
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn load_corpus() -> Vec<String> {
    let text =
        fs::read_to_string(fixtures_dir().join("lexer_mask_corpus.txt")).expect("corpus file");
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn mask_gold_snapshot() {
    let corpus = load_corpus();
    assert_eq!(corpus.len(), 31, "corpus must have exactly 31 queries");
    let actual: Vec<String> = corpus.iter().map(|s| mask_sql_literals(s)).collect();

    let expected_path = fixtures_dir().join("lexer_mask_expected.txt");

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

    assert_eq!(
        actual.len(),
        expected.len(),
        "line count mismatch: actual={}, expected={}",
        actual.len(),
        expected.len()
    );

    for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
        assert_eq!(
            a, e,
            "line {} mismatch\ninput:    {}\nactual:   {}\nexpected: {}",
            i, corpus[i], a, e
        );
    }
}
