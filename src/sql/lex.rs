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

impl<'a> SqlLexer<'a> {
    /// Scan one token and advance internal state. Returns (kind, start, end)
    /// of the token text, or None at EOF. Separated from `Iterator::next` so
    /// `visit_tokens` can avoid the Token struct allocation in hot paths.
    #[inline]
    fn advance(&mut self) -> Option<(TokenKind, usize, usize)> {
        let rest = self.src.get(self.pos..)?;
        if rest.is_empty() {
            return None;
        }
        let start = self.pos;
        let first = rest.chars().next()?;

        // Line comment: -- to end of line (or EOF)
        if rest.starts_with("--") {
            let end = rest
                .find('\n')
                .map(|off| start + off)
                .unwrap_or(start + rest.len());
            return Some(self.advance_emit(TokenKind::LineComment, start, end));
        }

        // Block comment: /* ... */ (non-nesting, matches current masking behavior)
        if rest.starts_with("/*") {
            let bytes = rest.as_bytes();
            let mut i = 2;
            while i + 1 < bytes.len() {
                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    return Some(self.advance_emit(TokenKind::BlockComment, start, start + i + 2));
                }
                i += 1;
            }
            // Unterminated — consume to EOF.
            return Some(self.advance_emit(TokenKind::BlockComment, start, start + rest.len()));
        }

        // Whitespace run
        if first.is_whitespace() {
            let bytes = rest.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                let b = bytes[i];
                if b < 0x80 {
                    // ASCII fast path
                    if b.is_ascii_whitespace() {
                        i += 1;
                        continue;
                    }
                    break;
                }
                // Non-ASCII cold path: check for Unicode whitespace
                let ch = rest[i..].chars().next().expect("non-empty non-ASCII slice");
                if ch.is_whitespace() {
                    i += ch.len_utf8();
                } else {
                    break;
                }
            }
            return Some(self.advance_emit(TokenKind::Whitespace, start, start + i));
        }

        // Single-quoted string literal (with '' escape)
        if first == '\'' {
            let end = scan_single_quoted(rest, start);
            return Some(self.advance_emit(TokenKind::StringLiteral, start, end));
        }

        // Dollar-quoted string: $$ or $tag$
        if first == '$' {
            if let Some(end) = scan_dollar_quoted(rest, start) {
                return Some(self.advance_emit(TokenKind::DollarString, start, end));
            }
            // Bind param? $1, $42 ...
            if let Some(end) = scan_bind_param(rest, start) {
                return Some(self.advance_emit(TokenKind::BindParam, start, end));
            }
            // Bare $ — treat as Punct.
            return Some(self.advance_emit(TokenKind::Punct, start, start + 1));
        }

        // Double-quoted identifier (with "" escape)
        if first == '"' {
            let end = scan_quoted_ident(rest, start);
            return Some(self.advance_emit(TokenKind::QuotedIdent, start, end));
        }

        // Numeric literal: optional leading '-' in numeric context, digits, optional
        // decimal part, optional scientific-notation suffix.
        if first.is_ascii_digit() || (first == '-' && self.is_numeric_context(rest)) {
            let end = scan_number(rest, start);
            return Some(self.advance_emit(TokenKind::Number, start, end));
        }

        // Identifier
        if first.is_alphabetic() || first == '_' {
            let bytes = rest.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                let b = bytes[i];
                if b < 0x80 {
                    // ASCII fast path
                    if b.is_ascii_alphanumeric() || b == b'_' {
                        i += 1;
                        continue;
                    }
                    break;
                }
                // Non-ASCII cold path: check for Unicode ident char
                let ch = rest[i..].chars().next().expect("non-empty non-ASCII slice");
                if ch.is_alphanumeric() || ch == '_' {
                    i += ch.len_utf8();
                } else {
                    break;
                }
            }
            return Some(self.advance_emit(TokenKind::Ident, start, start + i));
        }

        // Single-char punctuation fallback.
        let end = start + first.len_utf8();
        Some(self.advance_emit(TokenKind::Punct, start, end))
    }

    /// Update `pos` and `last_punct` per the token-kind rule (Whitespace
    /// transparent; Punct sets; anything else clears), then return the tuple.
    #[inline]
    fn advance_emit(
        &mut self,
        kind: TokenKind,
        start: usize,
        end: usize,
    ) -> (TokenKind, usize, usize) {
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
        (kind, start, end)
    }

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
            Some(p) => matches!(p, "(" | "," | "=" | "<" | ">" | "+" | "-" | "*" | "/" | "|"),
        }
    }
}

impl<'a> Iterator for SqlLexer<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let (kind, start, end) = self.advance()?;
        Some(Token {
            kind,
            text: &self.src[start..end],
            span: start..end,
        })
    }
}

/// Zero-alloc token visitor. Invokes `f(kind, text)` for each token in `sql`
/// without constructing `Token` values. Identical token boundaries to
/// iterating `SqlLexer::new(sql)`.
///
/// Use this in hot paths that only need `kind` + `text` (not `span`). Callers
/// that need positioned spans should use `SqlLexer::new(sql)` as an iterator.
///
/// # Stability
///
/// Internal-stable only. `pg-retest` is pre-1.0 and the signature may change
/// as Phase 2 / Phase 3 of the SQL parsing upgrade lands (e.g., to pass richer
/// context like enclosing-CTE or prior-keyword state). Do not rely on this
/// function from outside the crate.
#[inline]
pub fn visit_tokens<'a, F: FnMut(TokenKind, &'a str)>(sql: &'a str, mut f: F) {
    let mut lexer = SqlLexer::new(sql);
    while let Some((kind, start, end)) = lexer.advance() {
        f(kind, &sql[start..end]);
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_no_tokens() {
        let lex = SqlLexer::new("");
        assert_eq!(lex.count(), 0);
    }

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
        assert_eq!(kinds("(),;=<>+*/%|&"), vec![TokenKind::Punct; 13]);
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

    #[test]
    fn lex_line_comment() {
        assert_eq!(
            kinds("-- hello\nSELECT"),
            vec![
                TokenKind::LineComment,
                TokenKind::Whitespace,
                TokenKind::Ident
            ]
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

    #[test]
    fn lex_integer() {
        assert_eq!(kinds("42"), vec![TokenKind::Number]);
    }

    #[test]
    fn lex_bind_param_single() {
        assert_eq!(kinds("$1"), vec![TokenKind::BindParam]);
        assert_eq!(texts("$1"), vec!["$1"]);
    }

    #[test]
    fn lex_bind_param_multi_digit() {
        assert_eq!(kinds("$42"), vec![TokenKind::BindParam]);
        assert_eq!(texts("$42"), vec!["$42"]);
    }

    #[test]
    fn lex_bind_param_then_ident() {
        // Ensure $1 is BindParam (not an unterminated DollarString) when
        // followed by whitespace + ident.
        assert_eq!(
            kinds("$1 AS x"),
            vec![
                TokenKind::BindParam,
                TokenKind::Whitespace,
                TokenKind::Ident,
                TokenKind::Whitespace,
                TokenKind::Ident,
            ]
        );
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
}
