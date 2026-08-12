//! Lexer, AST, and recursive-descent parser for the Roller language.

mod ast;
mod lexer;
mod parser;

pub use ast::*;
pub use lexer::{LexError, Lexer, Token, TokenKind};
pub use parser::{ParseError, Parser};

/// Tokenize and parse a complete Roller source file.
pub fn parse(source: &str) -> Result<Program, FrontendError> {
    let tokens = Lexer::new(source).tokenize()?;
    Ok(Parser::new(tokens).parse_program()?)
}

/// Lexer or parser failure.
#[derive(Debug, thiserror::Error)]
pub enum FrontendError {
    /// Lexical analysis failed.
    #[error(transparent)]
    Lex(#[from] LexError),
    /// Parsing failed.
    #[error(transparent)]
    Parse(#[from] ParseError),
}
