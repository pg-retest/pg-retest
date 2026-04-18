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
        Token {
            kind,
            text,
            span: start..end,
        }
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
}
