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
        Token {
            kind,
            text,
            span: start..end,
        }
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
}
