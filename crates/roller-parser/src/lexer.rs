//! UTF-8 aware Roller lexer.

use crate::{Position, Span};

/// Token category.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    Import,
    Section,
    Let,
    If,
    Else,
    ForParallel,
    In,
    Parallel,
    True,
    False,
    Return,
    Library,
    Function,
    Compiler,
    Implement,
    Parallelable,
    Self_,
    Define,
    Identifier(String),
    Integer(u64),
    String(String),
    LeftBrace,
    RightBrace,
    LeftParen,
    RightParen,
    LeftBracket,
    RightBracket,
    Comma,
    Semicolon,
    Colon,
    DoubleColon,
    Dot,
    Ampersand,
    Bang,
    Arrow,
    EqualEqual,
    BangEqual,
    AndAnd,
    OrOr,
    Equal,
    Less,
    Greater,
    Eof,
}

impl TokenKind {
    /// Human-readable token description used in diagnostics.
    #[must_use]
    pub fn description(&self) -> String {
        match self {
            Self::Identifier(value) => format!("identifier `{value}`"),
            Self::Integer(value) => format!("integer `{value}`"),
            Self::String(_) => "string literal".into(),
            Self::Eof => "end of file".into(),
            other => format!("`{}`", other.spelling()),
        }
    }

    fn spelling(&self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Section => "section",
            Self::Let => "let",
            Self::If => "if",
            Self::Else => "else",
            Self::ForParallel => "for-parallel",
            Self::In => "in",
            Self::Parallel => "parallel",
            Self::True => "true",
            Self::False => "false",
            Self::Return => "return",
            Self::Library => "library",
            Self::Function => "function",
            Self::Compiler => "compiler",
            Self::Implement => "implement",
            Self::Parallelable => "paralleable",
            Self::Self_ => "self",
            Self::Define => "#define",
            Self::LeftBrace => "{",
            Self::RightBrace => "}",
            Self::LeftParen => "(",
            Self::RightParen => ")",
            Self::LeftBracket => "[",
            Self::RightBracket => "]",
            Self::Comma => ",",
            Self::Semicolon => ";",
            Self::Colon => ":",
            Self::DoubleColon => "::",
            Self::Dot => ".",
            Self::Ampersand => "&",
            Self::Bang => "!",
            Self::Arrow => "->",
            Self::EqualEqual => "==",
            Self::BangEqual => "!=",
            Self::AndAnd => "&&",
            Self::OrOr => "||",
            Self::Equal => "=",
            Self::Less => "<",
            Self::Greater => ">",
            Self::Identifier(_) | Self::Integer(_) | Self::String(_) | Self::Eof => "",
        }
    }
}

/// A token and its exact source range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Token category and decoded value.
    pub kind: TokenKind,
    /// Source range.
    pub span: Span,
}

/// A lexical failure with source context.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message} at {line}:{column}", line = span.start.line, column = span.start.column)]
pub struct LexError {
    /// Explanation of the invalid input.
    pub message: String,
    /// Source range containing the problem.
    pub span: Span,
}

/// Stateful lexer over one source string.
pub struct Lexer<'a> {
    source: &'a str,
    offset: usize,
    line: usize,
    column: usize,
}

impl<'a> Lexer<'a> {
    /// Create a lexer at the beginning of `source`.
    #[must_use]
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            offset: 0,
            line: 1,
            column: 1,
        }
    }

    /// Read all tokens, including a final EOF token.
    pub fn tokenize(mut self) -> Result<Vec<Token>, LexError> {
        let mut tokens = Vec::new();
        loop {
            self.skip_trivia()?;
            let start = self.position();
            let Some(character) = self.peek() else {
                tokens.push(Token {
                    kind: TokenKind::Eof,
                    span: Span { start, end: start },
                });
                return Ok(tokens);
            };

            let kind = if character.is_ascii_alphabetic() || character == '_' {
                self.lex_identifier()
            } else if character.is_ascii_digit() {
                self.lex_integer(start)?
            } else {
                self.lex_symbol_or_string(start)?
            };
            tokens.push(Token {
                kind,
                span: Span {
                    start,
                    end: self.position(),
                },
            });
        }
    }

    fn skip_trivia(&mut self) -> Result<(), LexError> {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.advance();
            }
            if self.remaining().starts_with("//") {
                while self.peek().is_some_and(|character| character != '\n') {
                    self.advance();
                }
            } else if self.remaining().starts_with("/*") {
                let start = self.position();
                self.advance();
                self.advance();
                while !self.remaining().starts_with("*/") {
                    if self.peek().is_none() {
                        return Err(LexError {
                            message: "unterminated block comment".into(),
                            span: Span {
                                start,
                                end: self.position(),
                            },
                        });
                    }
                    self.advance();
                }
                self.advance();
                self.advance();
            } else {
                return Ok(());
            }
        }
    }

    fn lex_identifier(&mut self) -> TokenKind {
        let start = self.offset;
        while self.peek().is_some_and(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '-'
        }) {
            self.advance();
        }
        match &self.source[start..self.offset] {
            "import" => TokenKind::Import,
            "section" => TokenKind::Section,
            "let" => TokenKind::Let,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "for-parallel" => TokenKind::ForParallel,
            "in" => TokenKind::In,
            "parallel" => TokenKind::Parallel,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "return" => TokenKind::Return,
            "library" => TokenKind::Library,
            "function" => TokenKind::Function,
            "compiler" => TokenKind::Compiler,
            "implement" => TokenKind::Implement,
            "paralleable" => TokenKind::Parallelable,
            "self" => TokenKind::Self_,
            value => TokenKind::Identifier(value.into()),
        }
    }

    fn lex_integer(&mut self, start: Position) -> Result<TokenKind, LexError> {
        let byte_start = self.offset;
        while self
            .peek()
            .is_some_and(|character| character.is_ascii_digit())
        {
            self.advance();
        }
        self.source[byte_start..self.offset]
            .parse()
            .map(TokenKind::Integer)
            .map_err(|_| LexError {
                message: "integer literal does not fit in an unsigned 64-bit value".into(),
                span: Span {
                    start,
                    end: self.position(),
                },
            })
    }

    fn lex_symbol_or_string(&mut self, start: Position) -> Result<TokenKind, LexError> {
        let remaining = self.remaining();
        for (spelling, kind) in [
            ("::", TokenKind::DoubleColon),
            ("->", TokenKind::Arrow),
            ("==", TokenKind::EqualEqual),
            ("!=", TokenKind::BangEqual),
            ("&&", TokenKind::AndAnd),
            ("||", TokenKind::OrOr),
        ] {
            if remaining.starts_with(spelling) {
                self.advance();
                self.advance();
                return Ok(kind);
            }
        }
        if remaining.starts_with("#define") {
            for _ in "#define".chars() {
                self.advance();
            }
            return Ok(TokenKind::Define);
        }
        if self.peek() == Some('"') {
            return self.lex_string(start);
        }
        let character = self.advance().ok_or_else(|| LexError {
            message: "unexpected end of input".into(),
            span: Span { start, end: start },
        })?;
        let kind = match character {
            '{' => TokenKind::LeftBrace,
            '}' => TokenKind::RightBrace,
            '(' => TokenKind::LeftParen,
            ')' => TokenKind::RightParen,
            '[' => TokenKind::LeftBracket,
            ']' => TokenKind::RightBracket,
            ',' => TokenKind::Comma,
            ';' => TokenKind::Semicolon,
            ':' => TokenKind::Colon,
            '.' => TokenKind::Dot,
            '&' => TokenKind::Ampersand,
            '!' => TokenKind::Bang,
            '=' => TokenKind::Equal,
            '<' => TokenKind::Less,
            '>' => TokenKind::Greater,
            _ => {
                return Err(LexError {
                    message: format!("unexpected character `{character}`"),
                    span: Span {
                        start,
                        end: self.position(),
                    },
                });
            }
        };
        Ok(kind)
    }

    fn lex_string(&mut self, start: Position) -> Result<TokenKind, LexError> {
        self.advance();
        let mut value = String::new();
        loop {
            let Some(character) = self.advance() else {
                return Err(LexError {
                    message: "unterminated string literal".into(),
                    span: Span {
                        start,
                        end: self.position(),
                    },
                });
            };
            match character {
                '"' => return Ok(TokenKind::String(value)),
                '\\' => {
                    let Some(escaped) = self.advance() else {
                        return Err(LexError {
                            message: "unterminated string escape".into(),
                            span: Span {
                                start,
                                end: self.position(),
                            },
                        });
                    };
                    value.push(match escaped {
                        'n' => '\n',
                        't' => '\t',
                        '"' => '"',
                        '\\' => '\\',
                        other => other,
                    });
                }
                other => value.push(other),
            }
        }
    }

    fn remaining(&self) -> &'a str {
        &self.source[self.offset..]
    }

    fn peek(&self) -> Option<char> {
        self.remaining().chars().next()
    }

    fn advance(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.offset += character.len_utf8();
        if character == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(character)
    }

    fn position(&self) -> Position {
        Position {
            offset: self.offset,
            line: self.line,
            column: self.column,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<TokenKind> {
        Lexer::new(source)
            .tokenize()
            .unwrap()
            .into_iter()
            .map(|token| token.kind)
            .collect()
    }

    #[test]
    fn recognizes_keywords_and_identifier() {
        assert_eq!(
            kinds("import section let if else for-parallel in parallel true false name"),
            vec![
                TokenKind::Import,
                TokenKind::Section,
                TokenKind::Let,
                TokenKind::If,
                TokenKind::Else,
                TokenKind::ForParallel,
                TokenKind::In,
                TokenKind::Parallel,
                TokenKind::True,
                TokenKind::False,
                TokenKind::Identifier("name".into()),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn distinguishes_symbols_and_reads_literals() {
        assert_eq!(
            kinds(": :: && \"hello\\nworld\" 42 #define"),
            vec![
                TokenKind::Colon,
                TokenKind::DoubleColon,
                TokenKind::AndAnd,
                TokenKind::String("hello\nworld".into()),
                TokenKind::Integer(42),
                TokenKind::Define,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn ignores_both_comment_forms() {
        assert_eq!(
            kinds("// comment\nfoo /* comment */ bar"),
            vec![
                TokenKind::Identifier("foo".into()),
                TokenKind::Identifier("bar".into()),
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn reports_invalid_character_with_position() {
        let error = Lexer::new("ok\n  @").tokenize().unwrap_err();
        assert_eq!(error.span.start.line, 2);
        assert_eq!(error.span.start.column, 3);
    }

    #[test]
    fn tokenizes_complete_sample() {
        let source = r#"import "gcc"
#define SRC "./src"
section build(jobs: int) {
 let cc = Compiler::new();
 if gcc::get_compiler(&cc) != Compiler::AVAILABLE && true {}
 for-parallel file in dir.recursive(SRC) { parallel cc.compile(file); }
}"#;
        assert!(Lexer::new(source).tokenize().unwrap().len() > 30);
    }
}
