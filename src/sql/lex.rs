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

#[allow(dead_code)] // fields populated by subsequent tasks that implement the iterator body
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
