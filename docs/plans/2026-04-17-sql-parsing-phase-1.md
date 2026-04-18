# SQL Parsing Upgrade — Phase 1: Shared SqlLexer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Introduce a shared `SqlLexer` in a new `src/sql/` module and migrate `mask_sql_literals` and `substitute_ids` to use it, eliminating duplicated character-scanning logic without regressing behavior or performance.

**Architecture:** New `src/sql/lex.rs` exposes a byte-offset-based `SqlLexer` iterator that emits typed `Token { kind, text, span }` values. Both masking (`src/capture/masking.rs`) and ID substitution (`src/correlate/substitute.rs`) get rewritten to consume the lexer stream instead of each running their own character state machine. Byte-for-byte output parity is enforced by pre-capture gold snapshot corpora; perf parity is enforced by criterion benches with pre-change baselines committed to the repo.

**Tech Stack:** Rust 2021, no new dependencies. Existing `criterion` (dev-dep) handles benchmarks. Existing `dashmap` consumed by `substitute_ids`.

**Mission Brief Anchor:** `/home/yonk/yonk-apps/pg-retest/skill-output/mission-brief/Mission-Brief-sql-parsing-upgrade.md` — re-read at every ⛔ drift checkpoint.

**Success Criteria covered by this plan:** SC-001, SC-002, SC-003, SC-004, SC-005 (partial — Phase 1 sites only), SC-010 (Phase 1 rc.4 release), SC-012.

**Drift Checkpoints injected:** DC-001 (after lexer, before caller migration) and DC-002 (end of Phase 1).

---

## File Structure

**New files:**
- `src/sql/mod.rs` — module root, re-exports `SqlLexer`, `Token`, `TokenKind`, `Span`.
- `src/sql/lex.rs` — `SqlLexer` iterator + token types + unit tests.
- `tests/fixtures/lexer_mask_corpus.txt` — 30-query corpus (one SQL per line, `#` lines are comments/skipped).
- `tests/fixtures/lexer_mask_expected.txt` — gold output captured from current `mask_sql_literals` before the rewrite.
- `tests/fixtures/lexer_substitute_corpus.txt` — 30-query corpus for substitute.
- `tests/fixtures/lexer_substitute_expected.txt` — gold output captured from current `substitute_ids` before the rewrite (each line is `"<count>|<output>"`).
- `tests/sql_lexer_mask_snapshot.rs` — integration test asserting `mask_sql_literals` matches the gold corpus; regenerates when `REGEN_SNAPSHOTS=1`.
- `tests/sql_lexer_substitute_snapshot.rs` — same pattern for substitute.
- `benches/mask_bench.rs` — new criterion bench for `mask_sql_literals`.
- `benches/baselines/substitute_before.txt` — human-readable criterion output captured against current impl.
- `benches/baselines/mask_before.txt` — same for new mask bench against current impl.

**Modified files:**
- `src/lib.rs` — add `pub mod sql;`.
- `src/capture/masking.rs` — rewrite `mask_sql_literals` on top of `SqlLexer`; keep public signature unchanged. Keep existing `#[cfg(test)] mod tests` block intact (all existing tests must still pass).
- `src/correlate/substitute.rs` — rewrite `substitute_ids` on top of `SqlLexer`; keep public signature unchanged. Preserve `Eligibility` state machine; only the token-scanning layer is replaced. Keep existing `#[cfg(test)] mod tests` block intact.
- `Cargo.toml` — add `[[bench]] name = "mask_bench" harness = false`.
- `CLAUDE.md` — add `src/sql/` entry to module list; note shared-lexer pattern in Gotchas.

**Files explicitly NOT touched in Phase 1:**
- `src/correlate/capture.rs` (`has_returning`, `inject_returning`) — Phase 2 scope, verified at DC-002.
- `src/transform/analyze.rs` (`extract_tables`, `extract_filter_columns`) — Phase 3 scope.
- `src/transform/mysql_to_pg.rs` — out of scope per brief.
- Proxy wire-protocol code (`src/proxy/connection.rs`, etc.) — out of scope per brief.

---

## Task 1: Capture performance baselines against current implementation

**Files:**
- Create: `benches/mask_bench.rs`
- Create: `benches/baselines/substitute_before.txt`
- Create: `benches/baselines/mask_before.txt`
- Modify: `Cargo.toml` (add `[[bench]]` entry for mask_bench)

- [ ] **Step 1: Add the mask bench file**

Create `benches/mask_bench.rs`:

```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use pg_retest::capture::masking::mask_sql_literals;

fn bench_mask_simple(c: &mut Criterion) {
    let sql = "SELECT * FROM users WHERE id = 42 AND name = 'Alice'";
    c.bench_function("mask_simple", |b| {
        b.iter(|| mask_sql_literals(black_box(sql)))
    });
}

fn bench_mask_many_literals(c: &mut Criterion) {
    let sql = "INSERT INTO orders (id, customer, price, status, note) VALUES \
               (1, 'Alice', 19.99, 'shipped', 'first order'), \
               (2, 'Bob', 42.50, 'pending', 'it''s urgent'), \
               (3, 'Carol', 7.25, 'cancelled', 'refund requested')";
    c.bench_function("mask_many_literals", |b| {
        b.iter(|| mask_sql_literals(black_box(sql)))
    });
}

fn bench_mask_no_literals(c: &mut Criterion) {
    let sql = "SELECT u.id, u.name, o.total FROM users u \
               INNER JOIN orders o ON o.user_id = u.id \
               WHERE u.active AND o.status = 'paid'";
    c.bench_function("mask_no_literals", |b| {
        b.iter(|| mask_sql_literals(black_box(sql)))
    });
}

fn bench_mask_dollar_quoted(c: &mut Criterion) {
    let sql = "SELECT $tag$hello 'world' with ''quotes''$tag$ FROM t";
    c.bench_function("mask_dollar_quoted", |b| {
        b.iter(|| mask_sql_literals(black_box(sql)))
    });
}

criterion_group!(
    benches,
    bench_mask_simple,
    bench_mask_many_literals,
    bench_mask_no_literals,
    bench_mask_dollar_quoted,
);
criterion_main!(benches);
```

- [ ] **Step 2: Register the bench in Cargo.toml**

Append to `Cargo.toml` (after the existing `[[bench]]` block for `substitute_bench`):

```toml
[[bench]]
name = "mask_bench"
harness = false
```

- [ ] **Step 3: Run both benches against current impl and capture output**

Create `benches/baselines/` directory and capture baseline output:

```bash
mkdir -p benches/baselines
cargo bench --bench substitute_bench 2>&1 | tee benches/baselines/substitute_before.txt
cargo bench --bench mask_bench 2>&1 | tee benches/baselines/mask_before.txt
```

Expected: each `.txt` file contains criterion's reported timings (e.g., `substitute_no_map  time:   [XXX ns YYY ns ZZZ ns]`).

- [ ] **Step 4: Verify baseline files are non-empty and contain timing data**

Run:

```bash
grep -E "time:" benches/baselines/substitute_before.txt | wc -l
grep -E "time:" benches/baselines/mask_before.txt | wc -l
```

Expected: `substitute_before.txt` reports 3 lines (3 bench cases), `mask_before.txt` reports 4 lines.

- [ ] **Step 5: Commit**

```bash
git add benches/mask_bench.rs benches/baselines/ Cargo.toml
git commit -m "bench: add mask_sql_literals bench and capture pre-change baselines

Establishes pre-Phase-1 perf baselines for substitute_ids and
mask_sql_literals so the shared-lexer migration can enforce no
regression (SC-004, SC-005)."
```

---

## Task 2: Scaffold `src/sql/` module and token types

**Files:**
- Create: `src/sql/mod.rs`
- Create: `src/sql/lex.rs`
- Modify: `src/lib.rs` (add `pub mod sql;`)

- [ ] **Step 1: Write the failing test**

Create `src/sql/lex.rs` with a single unit test asserting that an empty string yields zero tokens:

```rust
//! SQL lexical tokenizer shared by masking and ID substitution.
//!
//! Byte-offset based, zero-alloc per token, iterator pattern. Does not
//! attempt structural / grammatical parsing — just token boundaries.

use std::ops::Range;

pub type Span = Range<usize>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    Whitespace,
    Ident,
    StringLiteral,
    DollarString,
    QuotedIdent,
    Number,
    BindParam,
    LineComment,
    BlockComment,
    Punct,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token<'a> {
    pub kind: TokenKind,
    pub text: &'a str,
    pub span: Span,
}

pub struct SqlLexer<'a> {
    src: &'a str,
    pos: usize,
    /// Text of the most recent Punct token (if any) since the last non-punct
    /// non-whitespace token. Used for numeric-context detection: a leading `-`
    /// is treated as part of a Number only when the previous significant char
    /// was an operator-like punct (or start-of-input). Cleared when a non-Punct
    /// non-Whitespace token is emitted.
    last_punct: Option<&'a str>,
}

impl<'a> SqlLexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Self {
            src,
            pos: 0,
            last_punct: None,
        }
    }
}

impl<'a> Iterator for SqlLexer<'a> {
    type Item = Token<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_no_tokens() {
        let lex = SqlLexer::new("");
        assert_eq!(lex.count(), 0);
    }
}
```

- [ ] **Step 2: Create the module file**

Create `src/sql/mod.rs`:

```rust
pub mod lex;
pub use lex::{SqlLexer, Span, Token, TokenKind};
```

- [ ] **Step 3: Register the module in `src/lib.rs`**

Find the existing `pub mod` block near the top of `src/lib.rs` and add:

```rust
pub mod sql;
```

Place it alphabetically (after `pub mod replay;` or wherever it fits the existing order).

- [ ] **Step 4: Run the test**

```bash
cargo test --lib sql::lex::tests::empty_input_yields_no_tokens
```

Expected: PASS (the default `next() -> None` trivially satisfies the test).

- [ ] **Step 5: Verify clippy and format**

```bash
cargo clippy --lib -- -D warnings
cargo fmt --check
```

Expected: both clean.

- [ ] **Step 6: Commit**

```bash
git add src/sql/ src/lib.rs
git commit -m "feat(sql): scaffold SqlLexer module with token types

Introduces src/sql/lex.rs with Token, TokenKind, Span, and an empty
SqlLexer iterator. Subsequent tasks implement each token-kind branch
under TDD. Part of SC-001."
```

---

## Task 3: Lex whitespace, punctuation, and bare identifiers

**Files:**
- Modify: `src/sql/lex.rs`

- [ ] **Step 1: Write failing tests for whitespace, punct, ident**

Add to the `tests` module in `src/sql/lex.rs`:

```rust
fn kinds(sql: &str) -> Vec<TokenKind> {
    SqlLexer::new(sql).map(|t| t.kind).collect()
}

fn texts(sql: &str) -> Vec<&str> {
    let src = sql;
    SqlLexer::new(src).map(|t| &src[t.span]).collect()
}

#[test]
fn lex_whitespace() {
    assert_eq!(kinds("   \t\n "), vec![TokenKind::Whitespace]);
}

#[test]
fn lex_bare_identifier() {
    assert_eq!(kinds("foo"), vec![TokenKind::Ident]);
    assert_eq!(kinds("foo_bar"), vec![TokenKind::Ident]);
    assert_eq!(kinds("table3"), vec![TokenKind::Ident]);
}

#[test]
fn lex_punctuation() {
    assert_eq!(
        kinds("(),;=<>+*/%|&"),
        vec![TokenKind::Punct; 13]
    );
}

#[test]
fn lex_ident_then_ws_then_ident() {
    assert_eq!(
        kinds("foo bar"),
        vec![TokenKind::Ident, TokenKind::Whitespace, TokenKind::Ident]
    );
    assert_eq!(texts("foo bar"), vec!["foo", " ", "bar"]);
}

#[test]
fn lex_unicode_identifier() {
    // Non-ASCII letters are valid PG identifiers.
    let sql = "café";
    assert_eq!(kinds(sql), vec![TokenKind::Ident]);
}
```

- [ ] **Step 2: Run tests — verify they fail**

```bash
cargo test --lib sql::lex::tests 2>&1 | head -40
```

Expected: all four new tests FAIL (empty token stream).

- [ ] **Step 3: Implement whitespace, ident, and punct in the iterator**

Replace the `impl<'a> Iterator for SqlLexer<'a>` body:

```rust
impl<'a> Iterator for SqlLexer<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let rest = self.src.get(self.pos..)?;
        if rest.is_empty() {
            return None;
        }
        let start = self.pos;
        let first = rest.chars().next()?;

        // Whitespace run
        if first.is_whitespace() {
            let end = rest
                .char_indices()
                .take_while(|(_, c)| c.is_whitespace())
                .last()
                .map(|(i, c)| start + i + c.len_utf8())
                .unwrap_or(start + first.len_utf8());
            return Some(self.emit(TokenKind::Whitespace, start, end));
        }

        // Identifier (letter or underscore, then alnum/underscore)
        if first.is_alphabetic() || first == '_' {
            let end = rest
                .char_indices()
                .take_while(|(_, c)| c.is_alphanumeric() || *c == '_')
                .last()
                .map(|(i, c)| start + i + c.len_utf8())
                .unwrap_or(start + first.len_utf8());
            return Some(self.emit(TokenKind::Ident, start, end));
        }

        // Single-char punctuation fallback — everything not otherwise matched.
        let end = start + first.len_utf8();
        Some(self.emit(TokenKind::Punct, start, end))
    }
}

impl<'a> SqlLexer<'a> {
    fn emit(&mut self, kind: TokenKind, start: usize, end: usize) -> Token<'a> {
        let text = &self.src[start..end];
        self.pos = end;
        // Track the most recent Punct text so the Number branch can decide
        // whether a leading `-` is a negative sign. Whitespace is transparent;
        // any other non-punct significant token clears the punct memory.
        match kind {
            TokenKind::Whitespace => {}
            TokenKind::Punct => self.last_punct = Some(text),
            _ => self.last_punct = None,
        }
        Token { kind, text, span: start..end }
    }
}
```

- [ ] **Step 4: Run tests — verify they pass**

```bash
cargo test --lib sql::lex::tests
```

Expected: all whitespace/ident/punct tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/sql/lex.rs
git commit -m "feat(sql): lex whitespace, identifiers, punctuation

Adds first three token branches to SqlLexer. Unicode-aware
identifier handling matches the behavior added in f1a4cc7.
Part of SC-001."
```

---

## Task 4: Lex string literals (single-quoted, dollar-quoted, quoted-identifier)

**Files:**
- Modify: `src/sql/lex.rs`

- [ ] **Step 1: Write failing tests**

Add to the `tests` module:

```rust
#[test]
fn lex_single_quoted_string() {
    assert_eq!(kinds("'hello'"), vec![TokenKind::StringLiteral]);
    assert_eq!(texts("'hello'"), vec!["'hello'"]);
}

#[test]
fn lex_single_quoted_with_escape() {
    assert_eq!(kinds("'it''s'"), vec![TokenKind::StringLiteral]);
    assert_eq!(texts("'it''s'"), vec!["'it''s'"]);
}

#[test]
fn lex_unterminated_string_consumes_rest() {
    // Malformed input — lexer must not panic; consume remainder as StringLiteral.
    assert_eq!(kinds("'unterminated"), vec![TokenKind::StringLiteral]);
}

#[test]
fn lex_dollar_string_untagged() {
    assert_eq!(kinds("$$hello 'world'$$"), vec![TokenKind::DollarString]);
    assert_eq!(texts("$$hello 'world'$$"), vec!["$$hello 'world'$$"]);
}

#[test]
fn lex_dollar_string_tagged() {
    assert_eq!(kinds("$tag$body$tag$"), vec![TokenKind::DollarString]);
    assert_eq!(texts("$tag$body$tag$"), vec!["$tag$body$tag$"]);
}

#[test]
fn lex_dollar_string_nested_dollar() {
    // A $foo$ inside a $tag$ body should not close the tagged string.
    let sql = "$tag$a $foo$ b$tag$";
    assert_eq!(kinds(sql), vec![TokenKind::DollarString]);
    assert_eq!(texts(sql), vec![sql]);
}

#[test]
fn lex_quoted_identifier() {
    assert_eq!(kinds("\"my table\""), vec![TokenKind::QuotedIdent]);
}

#[test]
fn lex_quoted_identifier_with_escaped_quote() {
    // PG escapes double-quotes inside quoted idents by doubling them.
    let sql = "\"he said \"\"hi\"\"\"";
    assert_eq!(kinds(sql), vec![TokenKind::QuotedIdent]);
    assert_eq!(texts(sql), vec![sql]);
}
```

- [ ] **Step 2: Run tests — verify they fail**

```bash
cargo test --lib sql::lex::tests::lex_single_quoted_string sql::lex::tests::lex_dollar_string_untagged sql::lex::tests::lex_quoted_identifier
```

Expected: FAIL (these literals currently produce `Punct` tokens for `'`, `$`, `"`).

- [ ] **Step 3: Implement the string-lexing branches**

Modify the `next()` body — insert these branches **before** the `first.is_alphabetic()` check so `$` and `"` are caught first, and handle `'` before falling through to Punct. Full updated `next()`:

```rust
fn next(&mut self) -> Option<Self::Item> {
    let rest = self.src.get(self.pos..)?;
    if rest.is_empty() {
        return None;
    }
    let start = self.pos;
    let first = rest.chars().next()?;

    // Whitespace run
    if first.is_whitespace() {
        let end = rest
            .char_indices()
            .take_while(|(_, c)| c.is_whitespace())
            .last()
            .map(|(i, c)| start + i + c.len_utf8())
            .unwrap_or(start + first.len_utf8());
        return Some(self.emit(TokenKind::Whitespace, start, end));
    }

    // Single-quoted string literal (with '' escape)
    if first == '\'' {
        let end = scan_single_quoted(rest, start);
        return Some(self.emit(TokenKind::StringLiteral, start, end));
    }

    // Dollar-quoted string: $$ or $tag$
    if first == '$' {
        if let Some(end) = scan_dollar_quoted(rest, start) {
            return Some(self.emit(TokenKind::DollarString, start, end));
        }
        // Bind param? $1, $42 ...
        if let Some(end) = scan_bind_param(rest, start) {
            return Some(self.emit(TokenKind::BindParam, start, end));
        }
        // Bare $ — treat as Punct.
        return Some(self.emit(TokenKind::Punct, start, start + 1));
    }

    // Double-quoted identifier (with "" escape)
    if first == '"' {
        let end = scan_quoted_ident(rest, start);
        return Some(self.emit(TokenKind::QuotedIdent, start, end));
    }

    // Identifier
    if first.is_alphabetic() || first == '_' {
        let end = rest
            .char_indices()
            .take_while(|(_, c)| c.is_alphanumeric() || *c == '_')
            .last()
            .map(|(i, c)| start + i + c.len_utf8())
            .unwrap_or(start + first.len_utf8());
        return Some(self.emit(TokenKind::Ident, start, end));
    }

    // Single-char punctuation fallback.
    let end = start + first.len_utf8();
    Some(self.emit(TokenKind::Punct, start, end))
}
```

Add these free functions below the `impl` blocks:

```rust
/// Scan a single-quoted string starting at `rest[0]='\''`. Handles `''` escape.
/// Returns the absolute byte offset of the position AFTER the closing quote,
/// or end-of-input if unterminated.
fn scan_single_quoted(rest: &str, start: usize) -> usize {
    let bytes = rest.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        if bytes[i] == b'\'' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                i += 2;
                continue;
            }
            return start + i + 1;
        }
        i += 1;
    }
    start + bytes.len()
}

/// Scan a dollar-quoted string starting at `rest[0]='$'`.
/// Returns Some(end) if a valid dollar-quote was opened and closed (or ran to EOF),
/// None if the `$` is not an opening dollar-quote (e.g., `$1` bind param, `$,`).
fn scan_dollar_quoted(rest: &str, start: usize) -> Option<usize> {
    let bytes = rest.as_bytes();
    // Tag characters: alphanumeric + underscore.
    let mut tag_end = 1;
    while tag_end < bytes.len() {
        let b = bytes[tag_end];
        if b.is_ascii_alphanumeric() || b == b'_' {
            tag_end += 1;
        } else {
            break;
        }
    }
    if tag_end >= bytes.len() || bytes[tag_end] != b'$' {
        return None;
    }
    // The opening delimiter is bytes[0..=tag_end], e.g., "$$" or "$tag$".
    let delim = &rest[..=tag_end];
    let body_start = tag_end + 1;
    if let Some(close_rel) = rest[body_start..].find(delim) {
        Some(start + body_start + close_rel + delim.len())
    } else {
        // Unterminated — consume to EOF.
        Some(start + bytes.len())
    }
}

/// Scan a bind parameter `$1`, `$42`, ... starting at `rest[0]='$'`.
fn scan_bind_param(rest: &str, start: usize) -> Option<usize> {
    let bytes = rest.as_bytes();
    if bytes.len() < 2 || !bytes[1].is_ascii_digit() {
        return None;
    }
    let mut i = 2;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    Some(start + i)
}

/// Scan a double-quoted identifier starting at `rest[0]='"'`. Handles `""` escape.
fn scan_quoted_ident(rest: &str, start: usize) -> usize {
    let bytes = rest.as_bytes();
    let mut i = 1;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                i += 2;
                continue;
            }
            return start + i + 1;
        }
        i += 1;
    }
    start + bytes.len()
}
```

- [ ] **Step 4: Run tests — verify all pass**

```bash
cargo test --lib sql::lex::tests
```

Expected: all lex tests pass, including the new string/dollar/quoted-ident cases.

- [ ] **Step 5: Commit**

```bash
git add src/sql/lex.rs
git commit -m "feat(sql): lex string literals and dollar-quoted strings

Adds StringLiteral, DollarString (tagged + untagged), QuotedIdent,
BindParam token branches. Preserves tagged-dollar handling from
f1a4cc7. Part of SC-001."
```

---

## Task 5: Lex line and block comments

**Files:**
- Modify: `src/sql/lex.rs`

- [ ] **Step 1: Write failing tests**

Add to `tests` module:

```rust
#[test]
fn lex_line_comment() {
    assert_eq!(
        kinds("-- hello\nSELECT"),
        vec![TokenKind::LineComment, TokenKind::Whitespace, TokenKind::Ident]
    );
    assert_eq!(texts("-- hello\nSELECT"), vec!["-- hello", "\n", "SELECT"]);
}

#[test]
fn lex_line_comment_eof() {
    assert_eq!(kinds("-- end"), vec![TokenKind::LineComment]);
}

#[test]
fn lex_block_comment() {
    assert_eq!(kinds("/* hi */"), vec![TokenKind::BlockComment]);
    assert_eq!(texts("/* hi */"), vec!["/* hi */"]);
}

#[test]
fn lex_block_comment_unterminated() {
    // Malformed — consume to EOF without panic.
    assert_eq!(kinds("/* oops"), vec![TokenKind::BlockComment]);
}

#[test]
fn lex_block_comment_with_star_inside() {
    assert_eq!(kinds("/* 2 * 3 */"), vec![TokenKind::BlockComment]);
}

#[test]
fn lex_minus_not_comment() {
    // A single `-` should be Punct, not LineComment.
    assert_eq!(kinds("-x"), vec![TokenKind::Punct, TokenKind::Ident]);
}
```

- [ ] **Step 2: Run tests — verify they fail**

```bash
cargo test --lib sql::lex::tests::lex_line_comment sql::lex::tests::lex_block_comment
```

Expected: FAIL (currently `--` produces two Punct tokens, `/*` produces two Punct tokens).

- [ ] **Step 3: Implement comment branches**

Insert **before the whitespace check** in `next()` (so `--` and `/*` are caught before any other two-char interpretation):

```rust
// Line comment: -- to end of line (or EOF)
if rest.starts_with("--") {
    let end = rest
        .find('\n')
        .map(|off| start + off)
        .unwrap_or(start + rest.len());
    return Some(self.emit(TokenKind::LineComment, start, end));
}

// Block comment: /* ... */ (non-nesting, matches current masking behavior)
if rest.starts_with("/*") {
    let bytes = rest.as_bytes();
    let mut i = 2;
    while i + 1 < bytes.len() {
        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
            return Some(self.emit(TokenKind::BlockComment, start, start + i + 2));
        }
        i += 1;
    }
    // Unterminated — consume to EOF.
    return Some(self.emit(TokenKind::BlockComment, start, start + rest.len()));
}
```

- [ ] **Step 4: Run tests — verify all pass**

```bash
cargo test --lib sql::lex::tests
```

Expected: all tests pass, including `lex_minus_not_comment` (single `-` still produces Punct).

- [ ] **Step 5: Commit**

```bash
git add src/sql/lex.rs
git commit -m "feat(sql): lex line and block comments

Adds LineComment (--) and BlockComment (/* */) branches with
EOF-tolerant unterminated handling. Part of SC-001."
```

---

## Task 6: Lex numeric literals

**Files:**
- Modify: `src/sql/lex.rs`

- [ ] **Step 1: Write failing tests**

Add to `tests` module:

```rust
#[test]
fn lex_integer() {
    assert_eq!(kinds("42"), vec![TokenKind::Number]);
}

#[test]
fn lex_decimal() {
    assert_eq!(kinds("3.14"), vec![TokenKind::Number]);
    assert_eq!(texts("3.14"), vec!["3.14"]);
}

#[test]
fn lex_scientific() {
    assert_eq!(kinds("1.5e10"), vec![TokenKind::Number]);
    assert_eq!(kinds("2E-3"), vec![TokenKind::Number]);
}

#[test]
fn lex_negative_in_numeric_context() {
    // After `=`, a leading `-` is part of a negative number.
    assert_eq!(
        kinds("x=-5"),
        vec![TokenKind::Ident, TokenKind::Punct, TokenKind::Number]
    );
    assert_eq!(texts("x=-5"), vec!["x", "=", "-5"]);
}

#[test]
fn lex_subtract_not_negative() {
    // After an identifier (not an operator/punct), `-` is subtraction, not negative.
    assert_eq!(
        kinds("a - 5"),
        vec![
            TokenKind::Ident,
            TokenKind::Whitespace,
            TokenKind::Punct,
            TokenKind::Whitespace,
            TokenKind::Number,
        ]
    );
}

#[test]
fn lex_digit_after_ident_part_of_ident() {
    // `table3` is one Ident, not Ident+Number.
    assert_eq!(kinds("table3"), vec![TokenKind::Ident]);
}
```

- [ ] **Step 2: Run tests — verify they fail**

```bash
cargo test --lib sql::lex::tests::lex_integer sql::lex::tests::lex_decimal sql::lex::tests::lex_scientific sql::lex::tests::lex_negative_in_numeric_context
```

Expected: FAIL (digits currently caught by Punct fallback).

- [ ] **Step 3: Implement Number branch**

Insert in `next()` **after** the quoted-ident check and **before** the identifier check (so a leading digit is a Number, not attempted as an identifier):

```rust
// Numeric literal: optional leading '-' in numeric context, digits, optional
// decimal part, optional scientific-notation suffix.
if first.is_ascii_digit()
    || (first == '-' && self.is_numeric_context(rest))
{
    let end = scan_number(rest, start);
    return Some(self.emit(TokenKind::Number, start, end));
}
```

Add helper methods and a free function:

```rust
impl<'a> SqlLexer<'a> {
    /// Decide whether a `-` at `rest[0]` is a negative sign (part of a number)
    /// or a subtraction operator. True iff the next char is a digit AND the
    /// previous significant (non-whitespace) token was an operator-like
    /// single-char Punct (matches the char list from the pre-migration impl),
    /// or there was no prior significant token.
    fn is_numeric_context(&self, rest: &str) -> bool {
        let bytes = rest.as_bytes();
        if bytes.len() < 2 || !bytes[1].is_ascii_digit() {
            return false;
        }
        match self.last_punct {
            None => true,
            Some(p) => matches!(
                p,
                "(" | "," | "=" | "<" | ">" | "+" | "-" | "*" | "/" | "|"
            ),
        }
    }
}

/// Scan a numeric literal: `-?<digits>(.<digits>)?([eE][-+]?<digits>)?`.
/// Returns absolute end byte offset.
fn scan_number(rest: &str, start: usize) -> usize {
    let bytes = rest.as_bytes();
    let mut i = 0;
    if bytes[0] == b'-' {
        i = 1;
    }
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    // Decimal part
    if i < bytes.len() && bytes[i] == b'.' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit() {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }
    // Scientific notation
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let mut j = i + 1;
        if j < bytes.len() && (bytes[j] == b'+' || bytes[j] == b'-') {
            j += 1;
        }
        if j < bytes.len() && bytes[j].is_ascii_digit() {
            i = j;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    start + i
}
```

- [ ] **Step 4: Run full test suite for the lex module**

```bash
cargo test --lib sql::lex::tests
```

Expected: all tests pass, including the digit-after-ident test (that Ident branch still wins because we check `is_alphabetic() || '_'` before the Number branch — wait, update order: Number must come BEFORE identifier only for leading digits, but identifiers like `table3` must not start with a digit).

**Verify:** the `lex_digit_after_ident_part_of_ident` test passes because `table3` starts with `t` (alphabetic), so the identifier branch absorbs the whole run including trailing digits. The Number branch only fires on leading digit or leading `-digit` in numeric context.

- [ ] **Step 5: Commit**

```bash
git add src/sql/lex.rs
git commit -m "feat(sql): lex numeric literals

Adds Number branch: integers, decimals, scientific notation,
negative sign in operator-following numeric context. Part of SC-001."
```

---

## ⛔ Drift Check DC-001

**Trigger:** After SqlLexer is fully implemented (Tasks 2–6 complete), BEFORE any caller migration.

- [ ] **Step 1: Re-read mission brief**

```bash
cat /home/yonk/yonk-apps/pg-retest/skill-output/mission-brief/Mission-Brief-sql-parsing-upgrade.md
```

- [ ] **Step 2: Answer the three drift-detection questions**

Write your answers explicitly — do not skip. If any answer indicates drift, STOP and surface it to the user before proceeding.

1. **Am I still solving the stated Purpose?** Compare current work to the Purpose section of the brief. Expected answer: *yes — a shared SqlLexer module now exists, no caller has been touched, brittle structural parsing sites remain untouched for later phases.*
2. **Does my current work map to at least one Success Criterion?** Expected answer: *yes — SC-001 (SqlLexer module exists and emits all required token kinds). SC-004/005 baselines captured in Task 1.*
3. **Am I doing anything listed in Out of Scope?** Expected answer: *no — no changes to mysql_to_pg, wire-protocol, .wkl format, correlate error rate, CLI flags, `is_currval_or_lastval`, `has_returning`, `inject_returning`, or extractors.*

- [ ] **Step 3: Verify SC-001 evidence**

```bash
cargo test --lib sql::lex::tests 2>&1 | tail -5
```

Expected: all tests pass, at least ~20 test cases.

- [ ] **Step 4: Verify baselines captured (SC-004/005)**

```bash
ls -la benches/baselines/
wc -l benches/baselines/substitute_before.txt benches/baselines/mask_before.txt
```

Expected: both files exist and are non-empty.

- [ ] **Step 5: Verify no caller migration leaked in**

```bash
git diff HEAD~5 -- src/capture/masking.rs src/correlate/substitute.rs | wc -l
```

Expected: `0` (neither caller has been modified yet).

If the answer is non-zero, STOP — caller migration must not happen before snapshots are captured. Surface to the user.

---

## Task 7: Capture gold-snapshot corpus and migrate `mask_sql_literals`

**Files:**
- Create: `tests/fixtures/lexer_mask_corpus.txt`
- Create: `tests/fixtures/lexer_mask_expected.txt` (generated in this task)
- Create: `tests/sql_lexer_mask_snapshot.rs`

- [ ] **Step 1: Write the corpus file**

Create `tests/fixtures/lexer_mask_corpus.txt` with exactly 30 SQL queries, one per line (blank lines and `#`-prefixed lines are skipped by the test):

```
# lexer_mask_corpus.txt — gold-snapshot corpus for mask_sql_literals.
# One SQL statement per non-blank, non-# line.
# Regenerate expected.txt by running: REGEN_SNAPSHOTS=1 cargo test --test sql_lexer_mask_snapshot
SELECT 1
SELECT * FROM users WHERE id = 42
SELECT * FROM users WHERE name = 'Alice'
INSERT INTO t (s) VALUES ('it''s a test')
UPDATE orders SET price = 19.99 WHERE id = 7
DELETE FROM users WHERE email = 'x@y.z'
SELECT col1, col2 FROM table3
SELECT "column1" FROM "table2" WHERE id = 5
SELECT $$hello 'world'$$ FROM dual
SELECT $tag$body with 'quotes' and $$inside$$$tag$ FROM t
SELECT -5, 3.14, 1.5e10, 2E-3 FROM n
SELECT * FROM t WHERE x = -5
SELECT * FROM t WHERE x - 5 > 0
SELECT a + b, c * d FROM t
WITH cte AS (SELECT id FROM orders WHERE total > 100) SELECT * FROM cte
SELECT id, SUM(amount) OVER (PARTITION BY user_id ORDER BY ts) FROM payments
SELECT DISTINCT ON (user_id) user_id, id FROM events ORDER BY user_id, ts DESC
INSERT INTO t (a, b) VALUES ('hello', 42), ('world', 99)
SELECT count(*) FROM users WHERE created_at >= '2026-01-01'::date
SELECT data->>'email' FROM users WHERE data @> '{"active":true}'
SELECT * FROM t WHERE x IN (1, 2, 3) AND y NOT IN ('a', 'b')
SELECT * FROM t WHERE x BETWEEN 10 AND 20
-- top-level comment before SELECT
SELECT /* inline block */ 1 /* trailing */
SELECT 'line with -- fake comment inside string'
SELECT 'block /* not a comment */ in string'
INSERT INTO orders (id, note) VALUES (1, 'RETURNING id would be here if quoted')
SELECT * FROM "schema with spaces"."table with ""quotes"""
SELECT 'escaped '' quote' FROM t
SELECT e'raw\nstring' FROM t
UPDATE t SET col = NULL WHERE col IS NOT NULL
```

Note: that's 30 SQL lines (the `-- top-level comment before SELECT` line is a SQL line containing a PG comment, not a file comment — file comments use `#`). Count the non-blank, non-`#` lines to confirm.

- [ ] **Step 2: Write the snapshot test with regeneration support**

Create `tests/sql_lexer_mask_snapshot.rs`:

```rust
//! Gold-snapshot test: asserts mask_sql_literals produces byte-identical
//! output to a committed fixture. Regenerate the fixture when intentionally
//! changing mask behavior:
//!
//!     REGEN_SNAPSHOTS=1 cargo test --test sql_lexer_mask_snapshot
//!
//! Part of SC-002.

use pg_retest::capture::masking::mask_sql_literals;
use std::fs;
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn load_corpus() -> Vec<String> {
    let text = fs::read_to_string(fixtures_dir().join("lexer_mask_corpus.txt"))
        .expect("corpus file");
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|s| s.to_string())
        .collect()
}

#[test]
fn mask_gold_snapshot() {
    let corpus = load_corpus();
    assert_eq!(
        corpus.len(),
        30,
        "corpus must have exactly 30 queries"
    );
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
            i,
            corpus[i],
            a,
            e
        );
    }
}
```

- [ ] **Step 3: Regenerate expected.txt against CURRENT (pre-migration) impl**

```bash
REGEN_SNAPSHOTS=1 cargo test --test sql_lexer_mask_snapshot
```

Expected: test returns early with `regenerated .../lexer_mask_expected.txt`.

- [ ] **Step 4: Verify test passes against current impl**

```bash
cargo test --test sql_lexer_mask_snapshot
```

Expected: PASS (identical impl producing same output against its own gold).

- [ ] **Step 5: Rewrite `mask_sql_literals` on top of SqlLexer**

Replace the body of `mask_sql_literals` in `src/capture/masking.rs` (keep the doc comment). The helper functions `is_part_of_identifier` and `is_numeric_context` at the bottom of the file can be removed — they are no longer used.

New body:

```rust
/// Mask SQL literal values to prevent PII leakage.
///
/// Replaces single-quoted strings with `$S` and numeric literals with `$N`.
/// Handles escaped quotes (`''`), dollar-quoted strings (`$$...$$` and
/// `$tag$...$tag$`), and does not mask digits inside identifiers.
///
/// Reuses `crate::sql::SqlLexer` for token boundary detection.
pub fn mask_sql_literals(sql: &str) -> String {
    use crate::sql::{SqlLexer, TokenKind};

    let mut out = String::with_capacity(sql.len());
    for tok in SqlLexer::new(sql) {
        match tok.kind {
            TokenKind::StringLiteral | TokenKind::DollarString => out.push_str("$S"),
            TokenKind::Number => out.push_str("$N"),
            _ => out.push_str(tok.text),
        }
    }
    out
}
```

Then delete the now-unused `is_part_of_identifier` and `is_numeric_context` functions in the same file.

- [ ] **Step 6: Run the gold snapshot test and the existing unit tests**

```bash
cargo test --test sql_lexer_mask_snapshot
cargo test --lib capture::masking::tests
```

Expected: both pass. If the snapshot test fails on any line, **do not regenerate the snapshot** — investigate the lexer output or the mask function. The snapshot was captured against the pre-change impl specifically to catch deviations.

- [ ] **Step 7: Verify clippy and format**

```bash
cargo clippy --lib --tests -- -D warnings
cargo fmt --check
```

Expected: both clean.

- [ ] **Step 8: Commit**

```bash
git add src/capture/masking.rs tests/fixtures/lexer_mask_corpus.txt tests/fixtures/lexer_mask_expected.txt tests/sql_lexer_mask_snapshot.rs
git commit -m "refactor(capture): rewrite mask_sql_literals on SqlLexer

Replaces 100+ lines of hand-rolled character state machine with a
10-line fold over SqlLexer tokens. Gold-snapshot test (30 queries
including CTEs, windows, JSON ops, tagged dollar quotes, Unicode
idents) asserts byte-for-byte output parity with the pre-change
impl. Existing unit tests continue to pass.

SC-002."
```

---

## Task 8: Verify `mask_bench` shows no regression

**Files:**
- No file changes; this is a verification step.

- [ ] **Step 1: Run the post-change bench**

```bash
cargo bench --bench mask_bench 2>&1 | tee benches/baselines/mask_after.txt
```

- [ ] **Step 2: Compare against pre-change baseline**

Produce a side-by-side timing comparison:

```bash
paste <(grep -E "^mask_|time:" benches/baselines/mask_before.txt) \
      <(grep -E "^mask_|time:" benches/baselines/mask_after.txt)
```

Expected: each `time: [lo mid hi]` row — the `after` median should be **≤** the `before` median across all four bench cases (`mask_simple`, `mask_many_literals`, `mask_no_literals`, `mask_dollar_quoted`).

- [ ] **Step 3: Quantify delta**

Extract median ns for each case from both files and confirm `after <= before * 1.00` (no tolerance per mission brief SC-004/SC-005 — no regression). If any case regresses, investigate and fix before proceeding. Common fix targets:
- `SqlLexer::next()` making unnecessary `char_indices()` passes → switch to byte-level iteration.
- Extra allocations in `emit()` → ensure `text` is a `&str` borrow, not a copy.
- `String::with_capacity(sql.len())` under-estimating for expansion-heavy inputs (unlikely for mask which always shrinks or matches).

- [ ] **Step 4: Commit the post-change bench output**

```bash
git add benches/baselines/mask_after.txt
git commit -m "bench: capture post-change mask bench (no regression)

Documents mask_sql_literals performance after SqlLexer migration.
Meets SC-005 no-regression gate."
```

---

## Task 9: Capture gold-snapshot corpus and migrate `substitute_ids`

**Files:**
- Create: `tests/fixtures/lexer_substitute_corpus.txt`
- Create: `tests/fixtures/lexer_substitute_expected.txt`
- Create: `tests/sql_lexer_substitute_snapshot.rs`

- [ ] **Step 1: Write the corpus file**

Create `tests/fixtures/lexer_substitute_corpus.txt`. Each non-blank, non-`#` line is a SQL statement. A fixed `DashMap` is used for substitution: `{"42": "1042", "7": "1007", "99": "1099", "alice": "alice_new"}`.

```
# lexer_substitute_corpus.txt — gold snapshot for substitute_ids.
# Each non-blank, non-# line is one SQL statement. The test applies
# a fixed id_map: {"42": "1042", "7": "1007", "99": "1099", "alice": "alice_new"}
# Expected output format per line: <count>|<output-sql>
SELECT * FROM t WHERE id = 42
SELECT * FROM t WHERE id = 42 AND x = 99
SELECT * FROM t WHERE id = 42 LIMIT 42
SELECT * FROM t WHERE id = 42 OFFSET 42
SELECT * FROM t FETCH FIRST 42 ROWS ONLY
SELECT 42 FROM t AS 42
UPDATE t SET col = 42 WHERE id = 7
DELETE FROM t WHERE id IN (42, 7, 99)
SELECT * FROM t WHERE name = 'alice'
INSERT INTO t (id, name) VALUES (42, 'alice')
SELECT table42 FROM t WHERE id = 42
SELECT * FROM t -- WHERE id = 42 (commented out)
SELECT * FROM t /* WHERE id = 42 */ WHERE y = 99
SELECT '42' FROM t WHERE id = 42
SELECT $$string with 42$$ FROM t WHERE id = 42
SELECT * FROM t WHERE id BETWEEN 7 AND 42
WITH cte AS (SELECT id FROM t WHERE id = 42) SELECT * FROM cte WHERE id = 99
SELECT * FROM t WHERE id = 42 ORDER BY id LIMIT 7 OFFSET 99
SELECT * FROM orders WHERE cust_id = 42 AND status = 'alice'
UPDATE orders SET price = 42.99 WHERE id = 7
DELETE FROM t WHERE "column 42" = 42
SELECT * FROM t WHERE id = -42
SELECT * FROM t WHERE a = 42 AND b > 7
SELECT * FROM t WHERE x = 42; SELECT 99
SELECT * FROM t WHERE id = 42 FOR UPDATE
SELECT * FROM t WHERE id = 42 FOR SHARE SKIP LOCKED
INSERT INTO t (id) VALUES (42) ON CONFLICT (id) DO UPDATE SET id = 99
SELECT * FROM t JOIN u ON t.id = u.id WHERE t.x = 42
SELECT 42 AS id FROM t
SELECT * FROM t HAVING COUNT(*) > 42
```

30 SQL lines. Count: non-blank, non-`#` lines.

- [ ] **Step 2: Write the snapshot test with regeneration support**

Create `tests/sql_lexer_substitute_snapshot.rs`:

```rust
//! Gold-snapshot test: asserts substitute_ids produces byte-identical
//! output to a committed fixture. Format per line: `<count>|<output>`.
//!
//! Regenerate with:
//!
//!     REGEN_SNAPSHOTS=1 cargo test --test sql_lexer_substitute_snapshot
//!
//! Part of SC-003.

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
```

- [ ] **Step 3: Regenerate expected.txt against CURRENT (pre-migration) impl**

```bash
REGEN_SNAPSHOTS=1 cargo test --test sql_lexer_substitute_snapshot
```

Expected: test prints `regenerated .../lexer_substitute_expected.txt`.

- [ ] **Step 4: Verify test passes against current impl**

```bash
cargo test --test sql_lexer_substitute_snapshot
```

Expected: PASS.

- [ ] **Step 5: Rewrite `substitute_ids` on top of SqlLexer**

Replace the body of `substitute_ids` in `src/correlate/substitute.rs`, keeping the public signature `pub fn substitute_ids<'a>(sql: &'a str, map: &DashMap<String, String>) -> (Cow<'a, str>, usize)`. Keep the `Eligibility` enum defined at the top of the file. The `State` enum and all `State::*` branches go away (replaced by token-kind matching). The helper `is_part_of_identifier` is no longer needed.

New body:

```rust
pub fn substitute_ids<'a>(sql: &'a str, map: &DashMap<String, String>) -> (Cow<'a, str>, usize) {
    use crate::sql::{SqlLexer, TokenKind};

    if map.is_empty() {
        return (Cow::Borrowed(sql), 0);
    }

    let mut out = String::with_capacity(sql.len());
    let mut count = 0usize;
    let mut eligibility = Eligibility::Neutral;

    for tok in SqlLexer::new(sql) {
        match tok.kind {
            // String literals: substitute the inner contents (between quotes)
            // if eligible. The stored key is the raw inner text (including
            // any `''` escapes, matching prior behavior).
            TokenKind::StringLiteral => {
                let inner = strip_single_quotes(tok.text);
                let should_sub = matches!(
                    eligibility,
                    Eligibility::Neutral | Eligibility::Eligible
                );
                if should_sub {
                    if let Some(replacement) = map.get(inner) {
                        out.push('\'');
                        out.push_str(replacement.value());
                        out.push('\'');
                        count += 1;
                    } else {
                        out.push_str(tok.text);
                    }
                } else {
                    out.push_str(tok.text);
                }
                if eligibility != Eligibility::Neutral {
                    eligibility = Eligibility::Neutral;
                }
            }
            // Numeric literals: substitute whole-token text if eligible AND
            // the replacement is a safe-numeric-shaped string.
            TokenKind::Number => {
                let should_sub = matches!(
                    eligibility,
                    Eligibility::Neutral | Eligibility::Eligible
                );
                if should_sub {
                    if let Some(replacement) = map.get(tok.text) {
                        let safe = replacement.value().chars().all(|c| {
                            c.is_ascii_digit()
                                || matches!(c, '.' | '-' | 'e' | 'E' | '+')
                        });
                        if safe {
                            out.push_str(replacement.value());
                            count += 1;
                        } else {
                            out.push_str(tok.text);
                        }
                    } else {
                        out.push_str(tok.text);
                    }
                } else {
                    out.push_str(tok.text);
                }
                if eligibility != Eligibility::Neutral {
                    eligibility = Eligibility::Neutral;
                }
            }
            TokenKind::Ident => {
                let upper_buf = tok.text.to_ascii_uppercase();
                let upper = upper_buf.as_str();
                match upper {
                    "WHERE" | "AND" | "OR" | "ON" | "IN" | "VALUES" | "SET"
                    | "BETWEEN" | "HAVING" => {
                        eligibility = Eligibility::Eligible;
                    }
                    "LIMIT" | "OFFSET" | "FETCH" => {
                        eligibility = Eligibility::Ineligible;
                    }
                    "AS" | "FROM" | "JOIN" | "INTO" | "TABLE" | "INDEX" => {
                        eligibility = Eligibility::IdentifierContext;
                    }
                    _ => {
                        if eligibility == Eligibility::IdentifierContext {
                            eligibility = Eligibility::Neutral;
                        }
                    }
                }
                out.push_str(tok.text);
            }
            TokenKind::QuotedIdent => {
                if eligibility == Eligibility::IdentifierContext {
                    eligibility = Eligibility::Neutral;
                }
                out.push_str(tok.text);
            }
            TokenKind::Punct => {
                // Operators that open a comparison context (`=`, `<`, `>`, `<=`,
                // `>=`, `<>`, `!=`) set Eligible. The lexer emits these as
                // single-char Punct tokens; we update eligibility on the
                // terminating char. `=` always opens; `<`/`>` open; any
                // trailing `=`/`>` consolidates but eligibility is already set.
                match tok.text {
                    "=" | "<" | ">" => eligibility = Eligibility::Eligible,
                    _ => {}
                }
                out.push_str(tok.text);
            }
            // Whitespace, comments, dollar-strings, bind params pass through
            // without changing eligibility (comments inside a WHERE clause
            // preserve the prior context, matching current behavior).
            _ => out.push_str(tok.text),
        }
    }

    if count == 0 {
        (Cow::Borrowed(sql), 0)
    } else {
        (Cow::Owned(out), count)
    }
}

/// Strip the leading and trailing single quote from a StringLiteral token text.
/// Does NOT unescape `''` — the stored map key is expected to include the
/// raw escape (matching current behavior where the inner buffer accumulated
/// `''` verbatim).
fn strip_single_quotes(tok_text: &str) -> &str {
    let bytes = tok_text.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'' {
        &tok_text[1..tok_text.len() - 1]
    } else {
        // Unterminated string — strip only the leading quote.
        &tok_text[1..]
    }
}
```

Delete the now-unused `State` enum and `is_part_of_identifier` function.

- [ ] **Step 6: Run the gold snapshot test and the existing unit tests**

```bash
cargo test --test sql_lexer_substitute_snapshot
cargo test --lib correlate::substitute::tests
```

Expected: both pass. If the snapshot test fails, the diff will show the exact input and the divergence. Do **not** regenerate — investigate the rewrite. Likely culprits:
- An eligibility-setting punctuation sequence that the old hand-rolled code handled with multi-char absorption (`<=`, `>=`, `!=`, `<>`): the lexer emits these as two Punct tokens, and the eligibility is already set on the first one. Current behavior matches.
- A multi-statement input (`SELECT ...; SELECT ...`): the `;` is Punct and does not reset eligibility. Matches current behavior (no reset on `;` in the hand-rolled code either).

- [ ] **Step 7: Run the substitute criterion bench and check for no regression**

```bash
cargo bench --bench substitute_bench 2>&1 | tee benches/baselines/substitute_after.txt
paste <(grep "time:" benches/baselines/substitute_before.txt) \
      <(grep "time:" benches/baselines/substitute_after.txt)
```

Expected: `after <= before` for all three cases (`substitute_no_map`, `substitute_small_map`, `substitute_large_map`).

If regression: investigate. Common fix — `tok.text.to_ascii_uppercase()` allocates per ident; replace with a case-insensitive keyword match using `eq_ignore_ascii_case` inline.

- [ ] **Step 8: Verify clippy and format**

```bash
cargo clippy --lib --tests -- -D warnings
cargo fmt --check
```

Expected: both clean.

- [ ] **Step 9: Commit**

```bash
git add src/correlate/substitute.rs tests/fixtures/lexer_substitute_corpus.txt tests/fixtures/lexer_substitute_expected.txt tests/sql_lexer_substitute_snapshot.rs benches/baselines/substitute_after.txt
git commit -m "refactor(correlate): rewrite substitute_ids on SqlLexer

Replaces ~500 lines of char-level state machine with a token-driven
fold. Eligibility state machine (WHERE/AND/OR/LIMIT/OFFSET/AS/FROM
keyword context) preserved intact — only the lexical layer swapped.

Gold-snapshot test (30 queries incl. CTEs, windows, comments,
multi-statement, ON CONFLICT, BETWEEN, FOR UPDATE) asserts
byte-for-byte output parity with pre-change impl. criterion benches
show no regression.

SC-003, SC-004."
```

---

## Task 10: Update CLAUDE.md and run final verification sweep

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Update the module list in CLAUDE.md**

Find the "Key modules:" section in `CLAUDE.md`. Add this entry in the appropriate alphabetical position (after `src/replay/` entries, before `src/tls/`):

```
- `sql::lex` — Shared SQL lexer (`SqlLexer`, `Token`, `TokenKind`, `Span`). Byte-offset-based, zero-alloc-per-token, iterator pattern. Consumed by `capture::masking` and `correlate::substitute`. Does NOT attempt structural parsing — token boundaries only. See `docs/plans/2026-04-17-sql-parsing-phase-1.md`.
```

- [ ] **Step 2: Add a Gotchas entry**

In the "Gotchas" section of `CLAUDE.md`, add:

```
- SQL lexer (`src/sql/lex.rs`) is shared by masking and ID substitution. When adding a new call site that needs SQL tokenization, consume `SqlLexer` rather than re-implementing a character scanner. The lexer handles: single-quoted strings with `''` escape, dollar-quoted strings (`$$` and `$tag$`), double-quoted identifiers with `""` escape, line/block comments (EOF-tolerant), numeric literals (int/decimal/scientific/negative-in-numeric-context), bind params (`$N`), Unicode identifiers.
- Phase 1 parity is enforced by gold-snapshot tests in `tests/sql_lexer_mask_snapshot.rs` and `tests/sql_lexer_substitute_snapshot.rs`. Regenerate with `REGEN_SNAPSHOTS=1 cargo test --test <name>` only when intentionally changing mask/substitute behavior.
```

- [ ] **Step 3: Full verification sweep**

Run the complete verification stack:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --lib
cargo test --tests
cargo build --release
```

Expected: all pass / clean.

- [ ] **Step 4: Confirm nothing outside Phase 1 scope was touched**

```bash
git diff --stat $(git merge-base HEAD main)..HEAD -- \
    src/correlate/capture.rs \
    src/transform/analyze.rs \
    src/transform/mysql_to_pg.rs \
    src/proxy/
```

Expected: empty output (no changes to Phase 2/3 sites or proxy wire-protocol code).

- [ ] **Step 5: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: update CLAUDE.md for src/sql module and Phase 1 gotchas

Documents the shared SqlLexer's role and the gold-snapshot parity
tests. Concludes Phase 1 of the SQL parsing upgrade."
```

---

## ⛔ Drift Check DC-002

**Trigger:** After Task 10 completes, BEFORE claiming Phase 1 done.

- [ ] **Step 1: Re-read mission brief**

```bash
cat /home/yonk/yonk-apps/pg-retest/skill-output/mission-brief/Mission-Brief-sql-parsing-upgrade.md
```

- [ ] **Step 2: Drift-detection questions**

1. **Am I still solving the stated Purpose?** Expected: *yes — duplicated scanners eliminated, lexical code consolidated, brittle structural sites untouched (reserved for Phases 2 and 3).*
2. **Does my current work map to Success Criteria?** Walk through SC-001, SC-002, SC-003, SC-004, SC-005 (Phase-1 scope) and confirm evidence:
   - SC-001 → `cargo test --lib sql::lex::tests` passes (>20 cases).
   - SC-002 → `cargo test --test sql_lexer_mask_snapshot` passes; existing `capture::masking::tests` unit tests pass.
   - SC-003 → `cargo test --test sql_lexer_substitute_snapshot` passes; existing `correlate::substitute::tests` unit tests pass.
   - SC-004 → `benches/baselines/substitute_before.txt` vs `substitute_after.txt` shows no regression across all three cases.
   - SC-005 → `benches/baselines/mask_before.txt` vs `mask_after.txt` shows no regression across all four cases.
3. **Am I doing anything in Out of Scope?** Expected: *no — `has_returning`, `inject_returning`, `extract_tables`, `extract_filter_columns`, `mysql_to_pg`, wire-protocol, CLI flags, `.wkl` format, `is_currval_or_lastval` all untouched.*

- [ ] **Step 3: Artifact checklist**

For every Phase-1 Success Criterion, confirm a specific artifact exists:

| SC   | Evidence | Location |
|------|----------|----------|
| SC-001 | Lexer + 20+ unit tests | `src/sql/lex.rs` |
| SC-002 | Mask rewrite + gold snapshot | `src/capture/masking.rs`, `tests/sql_lexer_mask_snapshot.rs`, `tests/fixtures/lexer_mask_*.txt` |
| SC-003 | Substitute rewrite + gold snapshot | `src/correlate/substitute.rs`, `tests/sql_lexer_substitute_snapshot.rs`, `tests/fixtures/lexer_substitute_*.txt` |
| SC-004 | Substitute bench parity | `benches/baselines/substitute_before.txt`, `substitute_after.txt` |
| SC-005 | Mask bench parity | `benches/baselines/mask_before.txt`, `mask_after.txt`, `benches/mask_bench.rs` |
| SC-012 | Build / clippy / fmt clean | Task 10 Step 3 output |

If any row has a blank "Evidence" column, Phase 1 is not complete.

- [ ] **Step 4: If DC-002 passes, hand off to user**

State explicitly: "Phase 1 complete, all DC-002 checks pass, ready for PR review against `dev/1.0.0-rc.4`. Phase 2 (pg_query for has_returning/inject_returning) gets its own plan on a future `dev/1.0.0-rc.5` branch."

---

## Out-of-Scope Reminder (from Mission Brief)

The executing agent MUST NOT, during Phase 1:

- Touch `has_returning` or `inject_returning` in `src/correlate/capture.rs`.
- Touch `extract_tables` or `extract_filter_columns` in `src/transform/analyze.rs`.
- Touch `transform::mysql_to_pg` at all.
- Add `pg_query` or any other new dependency.
- Modify the `.wkl` profile format.
- Modify proxy wire-protocol code (pool, health, TLS, shutdown, socket, resource-limit).
- Add CLI flags or change public API surface.
- "Fix" `is_currval_or_lastval` or the `substitute_ids` 4-7% cross-session error rate.

If the temptation arises while migrating ("I notice `has_returning` has a bug I could fix while I'm here" / "the extract_tables regex is broken on this corpus line"), STOP — it's Phase 2/3 scope. Surface the observation to the user as a note for the next phase, do not fix it in this branch.
