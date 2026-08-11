//! Before/after benefit harness (Part B): regex pipeline vs. polyglot transpiler on
//! a corpus of MySQL→PostgreSQL queries. Classifies every result as
//! correct / skipped-as-unsupported(honest) / MISTRANSLATED, prints a table, and
//! asserts the honesty guarantee: the transpiler's mistranslation count is 0.
//!
//!   cargo test --features polyglot-transform --test polyglot_benefit_harness -- --nocapture
//!
//! "Mistranslated" = the engine EMITTED output (did not flag-and-skip) that is either
//! (a) not valid PostgreSQL (pg_query rejects it — fails loudly on replay) or
//! (b) silently semantically corrupt (changed a string literal it should not touch).
//! This is a *syntactic* + literal-preservation bar; it cannot catch behavioral
//! divergence of valid PG (e.g. MySQL vs PG CONCAT null semantics) — see spec §1.
#![cfg(feature = "polyglot-transform")]

use pg_retest::transform::mysql_to_pg::mysql_to_pg_pipeline;
use pg_retest::transform::polyglot::mysql_to_pg_polyglot_pipeline;
use pg_retest::transform::{is_valid_postgres, TransformPipeline, TransformResult};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    Correct,
    Skipped,
    Mistranslated,
}

impl Class {
    fn tag(self) -> &'static str {
        match self {
            Class::Correct => "correct",
            Class::Skipped => "skipped",
            Class::Mistranslated => "MISTRANSLATED",
        }
    }
}

struct Case {
    sql: &'static str,
    note: &'static str,
    /// A substring the faithful output MUST preserve (string-literal cases). `None`
    /// means "only require valid PG".
    must_keep: Option<&'static str>,
}

fn classify(result: &TransformResult, input: &str, must_keep: Option<&str>) -> (Class, String) {
    let output = match result {
        TransformResult::Skipped { reason } => return (Class::Skipped, reason.clone()),
        TransformResult::Unchanged => input.to_string(),
        TransformResult::Transformed(s) => s.clone(),
    };
    let valid = is_valid_postgres(&output);
    let faithful = must_keep.is_none_or(|m| output.contains(m));
    if !valid {
        (Class::Mistranslated, format!("invalid PG → {output}"))
    } else if !faithful {
        (
            Class::Mistranslated,
            format!("corrupted literal → {output}"),
        )
    } else {
        (Class::Correct, output)
    }
}

fn run(pipeline: &TransformPipeline, case: &Case) -> (Class, String) {
    classify(&pipeline.apply(case.sql), case.sql, case.must_keep)
}

#[test]
fn benefit_regex_vs_polyglot() {
    // Corpus = every existing mysql_to_pg test case PLUS constructs the 7 regex rules
    // cannot handle. `must_keep` marks the string-literal cases where the literal's
    // value must survive.
    let corpus = [
        // --- the existing regex-rule cases (parity) ---
        Case {
            sql: "SELECT `id`, `name` FROM `users`",
            note: "backtick identifiers",
            must_keep: None,
        },
        Case {
            sql: "SELECT * FROM t LIMIT 10, 20",
            note: "LIMIT offset,count",
            must_keep: None,
        },
        Case {
            sql: "SELECT IFNULL(name, 'unknown') FROM users",
            note: "IFNULL→COALESCE",
            must_keep: None,
        },
        Case {
            sql: "SELECT IF(status = 1, 'active', 'inactive') FROM users",
            note: "IF()→CASE",
            must_keep: None,
        },
        Case {
            sql: "SELECT UNIX_TIMESTAMP()",
            note: "UNIX_TIMESTAMP()",
            must_keep: None,
        },
        Case {
            sql: "SHOW VARIABLES LIKE 'version'",
            note: "MySQL internal (SHOW)",
            must_keep: None,
        },
        Case {
            sql: "SET NAMES utf8mb4",
            note: "MySQL internal (SET NAMES)",
            must_keep: None,
        },
        Case {
            sql: "SELECT id FROM users",
            note: "already PG-compatible",
            must_keep: None,
        },
        Case {
            sql: "INSERT INTO orders (user_id, total) VALUES (1, 99.99)",
            note: "plain DML",
            must_keep: None,
        },
        // --- constructs the regex rules cannot handle (the value-add) ---
        Case {
            sql: "SELECT 'IF(x,1,0)'",
            note: "IF( inside string literal [HEADLINE]",
            must_keep: Some("'IF(x,1,0)'"),
        },
        Case {
            sql: "SELECT 'IFNULL(a,b)' AS lit",
            note: "IFNULL( inside string literal",
            must_keep: Some("'IFNULL(a,b)'"),
        },
        Case {
            sql: "CREATE TABLE t (id INT AUTO_INCREMENT PRIMARY KEY)",
            note: "AUTO_INCREMENT",
            must_keep: None,
        },
        Case {
            sql: "INSERT INTO t (id) VALUES (1) ON DUPLICATE KEY UPDATE id = id",
            note: "ON DUPLICATE KEY UPDATE",
            must_keep: None,
        },
        Case {
            sql: "SELECT name FROM t WHERE x = 'a\\'b'",
            note: "backslash string escape",
            must_keep: None,
        },
        Case {
            sql: "SELECT DATE_FORMAT(created_at, '%Y-%m-%d') FROM t",
            note: "DATE_FORMAT",
            must_keep: None,
        },
        Case {
            sql: "SELECT CONCAT(first, ' ', last) FROM users",
            note: "CONCAT",
            must_keep: None,
        },
        Case {
            sql: "SELECT GROUP_CONCAT(name SEPARATOR ',') FROM t",
            note: "GROUP_CONCAT",
            must_keep: None,
        },
    ];

    let regex = mysql_to_pg_pipeline();
    let poly = mysql_to_pg_polyglot_pipeline();

    let mut r_correct = 0;
    let mut r_skip = 0;
    let mut r_mis = 0;
    let mut p_correct = 0;
    let mut p_skip = 0;
    let mut p_mis = 0;

    println!("\n  MySQL → PostgreSQL: regex pipeline vs. polyglot transpiler\n");
    println!(
        "  {:<40} | {:<13} | {:<13}",
        "construct", "regex", "polyglot"
    );
    println!("  {}", "-".repeat(40 + 3 + 13 + 3 + 13));

    let mut detail_lines = Vec::new();
    for case in &corpus {
        let (rc, rd) = run(&regex, case);
        let (pc, pd) = run(&poly, case);
        match rc {
            Class::Correct => r_correct += 1,
            Class::Skipped => r_skip += 1,
            Class::Mistranslated => r_mis += 1,
        }
        match pc {
            Class::Correct => p_correct += 1,
            Class::Skipped => p_skip += 1,
            Class::Mistranslated => p_mis += 1,
        }
        println!("  {:<40} | {:<13} | {:<13}", case.note, rc.tag(), pc.tag());
        if rc == Class::Mistranslated {
            detail_lines.push(format!("    regex    {:<38} {}", case.note, rd));
        }
        if pc == Class::Mistranslated {
            detail_lines.push(format!("    polyglot {:<38} {}", case.note, pd));
        }
    }

    println!("\n  totals ({} constructs):", corpus.len());
    println!(
        "    regex    → correct {r_correct:>2} | skipped {r_skip:>2} | MISTRANSLATED {r_mis:>2}"
    );
    println!(
        "    polyglot → correct {p_correct:>2} | skipped {p_skip:>2} | MISTRANSLATED {p_mis:>2}"
    );
    if !detail_lines.is_empty() {
        println!("\n  mistranslation details:");
        for l in &detail_lines {
            println!("{l}");
        }
    }
    println!();

    // --- THE HONESTY GUARANTEE (headline benefit) ---
    assert_eq!(
        p_mis, 0,
        "polyglot transpiler must never mistranslate (flag-and-skip instead); had {p_mis}"
    );
    // The transpiler demonstrably avoids failures the brittle regex rules commit.
    assert!(
        r_mis >= 1,
        "expected the regex pipeline to mistranslate at least one construct the AST transpiler handles honestly"
    );
    // Pin the headline case explicitly: regex silently rewrites text inside a string
    // literal; polyglot preserves it.
    let headline = &corpus[9];
    assert_eq!(run(&regex, headline).0, Class::Mistranslated);
    assert_ne!(run(&poly, headline).0, Class::Mistranslated);
}
