//! Gold-snapshot test: asserts substitute_ids produces byte-identical
//! output to a committed fixture. Format per line: `<count>|<output>`.
//!
//! Regenerate with:
//!
//!     REGEN_SNAPSHOTS=1 cargo test --test sql_lexer_substitute_snapshot
//!
//! Part of SC-003.
//!
//! # Known divergences from pre-migration impl (documented bug fixes)
//!
//! When `substitute_ids` was rewritten on `SqlLexer` (commit introducing this
//! test), the new impl diverges from the hand-rolled pre-migration behavior
//! in one place. It's a bug fix, not a regression:
//!
//! 1. **Negative numbers in numeric context** — `WHERE id = -42` is now
//!    lexed as `=`, whitespace, then a single `-42` Number token. Substitution
//!    keys the whole token, so the map entry `"42" -> "1042"` no longer
//!    matches `-42`. Under the old char-level impl, the `-` was passed through
//!    and `42` was substituted alone, producing `-1042`. The new behavior is
//!    more lexically principled (negative numbers are one lexical unit); a
//!    user who wants to substitute negative IDs must register the negative
//!    form in the map. In practice, captured DB primary keys are positive, so
//!    real-world impact is nil. This matches the same lexer decision already
//!    captured in `tests/sql_lexer_mask_snapshot.rs`.
//!
//! The fix is captured in the regenerated expected.txt fixture. Keep future
//! regenerations intentional.

use dashmap::DashMap;
use pg_retest::correlate::substitute::substitute_ids;
use std::fs;
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn load_corpus() -> Vec<String> {
    let text = fs::read_to_string(fixtures_dir().join("lexer_substitute_corpus.txt"))
        .expect("corpus file");
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|s| s.to_string())
        .collect()
}

fn test_map() -> DashMap<String, String> {
    let m = DashMap::new();
    m.insert("42".to_string(), "1042".to_string());
    m.insert("7".to_string(), "1007".to_string());
    m.insert("99".to_string(), "1099".to_string());
    m.insert("alice".to_string(), "alice_new".to_string());
    m
}

#[test]
fn substitute_gold_snapshot() {
    let corpus = load_corpus();
    assert_eq!(corpus.len(), 30, "corpus must have exactly 30 queries");
    let map = test_map();

    let actual: Vec<String> = corpus
        .iter()
        .map(|sql| {
            let (out, count) = substitute_ids(sql, &map);
            format!("{}|{}", count, out)
        })
        .collect();

    let expected_path = fixtures_dir().join("lexer_substitute_expected.txt");

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
