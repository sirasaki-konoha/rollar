//! Hand-written recursive-descent parser.

use crate::{
    BinaryOperator, Block, CompilerDeclaration, ConstantDeclaration, Expression, ExpressionKind,
    FieldDeclaration, FunctionDeclaration, ImplementBlock, ImportDeclaration, LibraryDeclaration,
    LibraryItem, Parameter, Position, Program, SectionDeclaration, Span, Statement, Token,
    TokenKind, TopLevelItem,
};

/// A syntax failure with expected and actual tokens.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "expected {expected}, found {actual} at {line}:{column}",
    line = span.start.line,
    column = span.start.column
)]
pub struct ParseError {
    /// Expected grammar element.
    pub expected: String,
    /// Actual token description.
    pub actual: String,
    /// Source range containing the actual token.
    pub span: Span,
}

/// Recursive-descent parser over a token vector.
pub struct Parser {
    tokens: Vec<Token>,
    current: usize,
}

impl Parser {
    /// Create a parser. The input should end in an EOF token.
    #[must_use]
    pub fn new(mut tokens: Vec<Token>) -> Self {
        let needs_eof = tokens
            .last()
            .is_none_or(|token| !matches!(token.kind, TokenKind::Eof));
        if needs_eof {
            let position = tokens.last().map_or(
                Position {
                    offset: 0,
                    line: 1,
                    column: 1,
                },
                |token| token.span.end,
            );
            tokens.push(Token {
                kind: TokenKind::Eof,
                span: Span {
                    start: position,
                    end: position,
                },
            });
        }
        Self { tokens, current: 0 }
    }

    /// Parse a complete source file.
    pub fn parse_program(mut self) -> Result<Program, ParseError> {
        let start = self.peek().span;
        let mut items = Vec::new();
        while !self.at(&TokenKind::Eof) {
            items.push(self.parse_top_level()?);
        }
        let end = self.peek().span;
        let span = items
            .first()
            .map_or(start.join(end), |item| item.span().join(end));
        Ok(Program { items, span })
    }

    fn parse_top_level(&mut self) -> Result<TopLevelItem, ParseError> {
        if self.at(&TokenKind::Import) {
            self.parse_import().map(TopLevelItem::Import)
        } else if self.at(&TokenKind::Define) {
            self.parse_constant().map(TopLevelItem::Constant)
        } else if self.at(&TokenKind::Section) {
            self.parse_section().map(TopLevelItem::Section)
        } else if self.at(&TokenKind::Library) {
            self.parse_library().map(TopLevelItem::Library)
        } else {
            Err(self.error("`import`, `#define`, `section`, or `library`"))
        }
    }

    fn parse_import(&mut self) -> Result<ImportDeclaration, ParseError> {
        let start = self.consume(&TokenKind::Import, "`import`")?.span;
        let token = self.advance();
        let TokenKind::String(module) = token.kind else {
            return Err(ParseError {
                expected: "string literal".into(),
                actual: token.kind.description(),
                span: token.span,
            });
        };
        let end = if self.match_token(&TokenKind::Semicolon) {
            self.previous().span
        } else {
            token.span
        };
        Ok(ImportDeclaration {
            module,
            span: start.join(end),
        })
    }

    fn parse_constant(&mut self) -> Result<ConstantDeclaration, ParseError> {
        let start = self.consume(&TokenKind::Define, "`#define`")?.span;
        let (name, _) = self.consume_identifier("constant name")?;
        let value = self.parse_expression()?;
        let end = if self.match_token(&TokenKind::Semicolon) {
            self.previous().span
        } else {
            value.span
        };
        Ok(ConstantDeclaration {
            name,
            value,
            span: start.join(end),
        })
    }

    fn parse_section(&mut self) -> Result<SectionDeclaration, ParseError> {
        let start = self.consume(&TokenKind::Section, "`section`")?.span;
        let (name, _) = self.consume_identifier("section name")?;
        self.consume(&TokenKind::LeftParen, "`(`")?;
        let mut parameters = Vec::new();
        if !self.at(&TokenKind::RightParen) {
            loop {
                let (parameter_name, parameter_start) =
                    self.consume_identifier("parameter name")?;
                self.consume(&TokenKind::Colon, "`:`")?;
                let (type_name, type_span) = self.consume_identifier("type name")?;
                parameters.push(Parameter {
                    name: parameter_name,
                    type_name,
                    span: parameter_start.join(type_span),
                });
                if !self.match_token(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.consume(&TokenKind::RightParen, "`)`")?;
        let body = self.parse_block()?;
        Ok(SectionDeclaration {
            name,
            parameters,
            span: start.join(body.span),
            body,
        })
    }

    fn parse_library(&mut self) -> Result<LibraryDeclaration, ParseError> {
        let start = self.consume(&TokenKind::Library, "`library`")?.span;
        let token = self.advance();
        let TokenKind::String(name) = token.kind else {
            return Err(ParseError {
                expected: "string literal".into(),
                actual: token.kind.description(),
                span: token.span,
            });
        };
        self.consume(&TokenKind::LeftBrace, "`{`")?;
        let mut items = Vec::new();
        while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
            if self.at(&TokenKind::Compiler) {
                items.push(LibraryItem::Compiler(self.parse_compiler()?));
            } else if self.at(&TokenKind::Implement) {
                items.push(LibraryItem::Implement(self.parse_implement()?));
            } else {
                let is_parallelable = self.match_token(&TokenKind::Parallelable);
                let mut func = self.parse_function()?;
                func.is_parallelable = is_parallelable;
                items.push(LibraryItem::Function(func));
            }
        }
        let end = self.consume(&TokenKind::RightBrace, "`}`")?.span;
        Ok(LibraryDeclaration {
            name,
            items,
            span: start.join(end),
        })
    }

    fn parse_compiler(&mut self) -> Result<CompilerDeclaration, ParseError> {
        let start = self.consume(&TokenKind::Compiler, "`compiler`")?.span;
        let (name, _) = self.consume_identifier("compiler name")?;
        self.consume(&TokenKind::LeftBrace, "`{`")?;
        let mut fields = Vec::new();
        while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
            let (field_name, field_start) = self.consume_identifier("field name")?;
            self.consume(&TokenKind::Colon, "`:`")?;
            let (type_name, type_span) = self.parse_type_name()?;
            fields.push(FieldDeclaration {
                name: field_name,
                type_name,
                span: field_start.join(type_span),
            });
            if !self.match_token(&TokenKind::Comma) {
                let _ = self.match_token(&TokenKind::Semicolon);
            }
        }
        let end = self.consume(&TokenKind::RightBrace, "`}`")?.span;
        Ok(CompilerDeclaration {
            name,
            fields,
            span: start.join(end),
        })
    }

    fn parse_implement(&mut self) -> Result<ImplementBlock, ParseError> {
        let start = self.consume(&TokenKind::Implement, "`implement`")?.span;
        let (namespace, _) = self.consume_identifier("`Self`")?;
        if namespace != "Self" {
            return Err(ParseError {
                expected: "`Self`".into(),
                actual: format!("identifier `{namespace}`"),
                span: self.previous().span,
            });
        }
        self.consume(&TokenKind::DoubleColon, "`::`")?;
        let (compiler_name, _) = self.consume_identifier("compiler name")?;
        self.consume(&TokenKind::LeftBrace, "`{`")?;
        let mut methods = Vec::new();
        while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
            let is_parallelable = self.match_token(&TokenKind::Parallelable);
            let mut function = self.parse_function()?;
            function.is_parallelable = is_parallelable;
            methods.push(function);
        }
        let end = self.consume(&TokenKind::RightBrace, "`}`")?.span;
        Ok(ImplementBlock {
            compiler_name,
            methods,
            span: start.join(end),
        })
    }

    fn parse_type_name(&mut self) -> Result<(String, Span), ParseError> {
        let (mut name, start) = self.consume_identifier("type name")?;
        let mut end = start;
        if self.match_token(&TokenKind::Less) {
            let (argument, argument_span) = self.consume_identifier("type argument")?;
            end = self.consume(&TokenKind::Greater, "`>`")?.span;
            name.push('<');
            name.push_str(&argument);
            name.push('>');
            let _ = argument_span;
        }
        Ok((name, start.join(end)))
    }

    fn parse_function(&mut self) -> Result<FunctionDeclaration, ParseError> {
        let start = self.consume(&TokenKind::Function, "`function`")?.span;
        let (name, _) = self.consume_identifier("function name")?;
        self.consume(&TokenKind::LeftParen, "`(`")?;
        let mut parameters = Vec::new();
        if !self.at(&TokenKind::RightParen) {
            loop {
                let (parameter_name, parameter_start) =
                    self.consume_identifier("parameter name")?;
                let (type_name, type_end) = if self.match_token(&TokenKind::Colon) {
                    self.parse_type_name()?
                } else {
                    ("any".into(), parameter_start)
                };
                parameters.push(Parameter {
                    name: parameter_name,
                    type_name,
                    span: parameter_start.join(type_end),
                });
                if !self.match_token(&TokenKind::Comma) {
                    break;
                }
            }
        }
        self.consume(&TokenKind::RightParen, "`)`")?;
        let return_type = if self.match_token(&TokenKind::Arrow) {
            let (type_name, _) = self.parse_type_name()?;
            Some(type_name)
        } else {
            None
        };
        let body = self.parse_block()?;
        let span = start.join(body.span);
        Ok(FunctionDeclaration {
            name,
            parameters,
            return_type,
            body,
            is_parallelable: false,
            span,
        })
    }

    fn parse_block(&mut self) -> Result<Block, ParseError> {
        let start = self.consume(&TokenKind::LeftBrace, "`{`")?.span;
        let mut statements = Vec::new();
        while !self.at(&TokenKind::RightBrace) && !self.at(&TokenKind::Eof) {
            statements.push(self.parse_statement()?);
        }
        let end = self.consume(&TokenKind::RightBrace, "`}`")?.span;
        Ok(Block {
            statements,
            span: start.join(end),
        })
    }

    fn parse_statement(&mut self) -> Result<Statement, ParseError> {
        if self.at(&TokenKind::Let) {
            self.parse_let()
        } else if self.at(&TokenKind::If) {
            self.parse_if()
        } else if self.at(&TokenKind::ForParallel) {
            self.parse_for_parallel()
        } else if self.at(&TokenKind::Parallel) {
            self.parse_parallel()
        } else if self.at(&TokenKind::Return) {
            self.parse_return()
        } else {
            self.parse_expression_statement()
        }
    }

    fn parse_let(&mut self) -> Result<Statement, ParseError> {
        let start = self.consume(&TokenKind::Let, "`let`")?.span;
        let (name, _) = self.consume_identifier("binding name")?;
        self.consume(&TokenKind::Equal, "`=`")?;
        let value = self.parse_expression()?;
        let end = self.consume(&TokenKind::Semicolon, "`;`")?.span;
        Ok(Statement::Let {
            name,
            value,
            span: start.join(end),
        })
    }

    fn parse_if(&mut self) -> Result<Statement, ParseError> {
        let start = self.consume(&TokenKind::If, "`if`")?.span;
        let condition = self.parse_expression()?;
        let then_block = self.parse_block()?;
        let else_block = if self.match_token(&TokenKind::Else) {
            Some(self.parse_block()?)
        } else {
            None
        };
        let end = else_block
            .as_ref()
            .map_or(then_block.span, |block| block.span);
        Ok(Statement::If {
            condition,
            then_block,
            else_block,
            span: start.join(end),
        })
    }

    fn parse_for_parallel(&mut self) -> Result<Statement, ParseError> {
        let start = self
            .consume(&TokenKind::ForParallel, "`for-parallel`")?
            .span;
        let (binding, _) = self.consume_identifier("iteration variable")?;
        self.consume(&TokenKind::In, "`in`")?;
        let iterable = self.parse_expression()?;
        let body = self.parse_block()?;
        Ok(Statement::ForParallel {
            binding,
            iterable,
            span: start.join(body.span),
            body,
        })
    }

    fn parse_parallel(&mut self) -> Result<Statement, ParseError> {
        let start = self.consume(&TokenKind::Parallel, "`parallel`")?.span;
        let expression = self.parse_expression()?;
        let end = self.consume(&TokenKind::Semicolon, "`;`")?.span;
        Ok(Statement::Parallel {
            expression,
            span: start.join(end),
        })
    }

    fn parse_return(&mut self) -> Result<Statement, ParseError> {
        let start = self.consume(&TokenKind::Return, "`return`")?.span;
        let value = self.parse_expression()?;
        let end = if self.match_token(&TokenKind::Semicolon) {
            self.previous().span
        } else {
            value.span
        };
        Ok(Statement::Return {
            value,
            span: start.join(end),
        })
    }

    fn parse_expression_statement(&mut self) -> Result<Statement, ParseError> {
        let expression = self.parse_expression()?;
        if self.match_token(&TokenKind::Equal) {
            let value = self.parse_expression()?;
            let end = self.consume(&TokenKind::Semicolon, "`;`")?.span;
            return Ok(Statement::Assignment {
                span: expression.span.join(end),
                target: expression,
                value,
            });
        }
        let end = self.consume(&TokenKind::Semicolon, "`;`")?.span;
        Ok(Statement::Expression {
            span: expression.span.join(end),
            expression,
        })
    }

    fn parse_expression(&mut self) -> Result<Expression, ParseError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expression, ParseError> {
        let mut expression = self.parse_and()?;
        while self.match_token(&TokenKind::OrOr) {
            let right = self.parse_and()?;
            expression = binary(expression, BinaryOperator::Or, right);
        }
        Ok(expression)
    }

    fn parse_and(&mut self) -> Result<Expression, ParseError> {
        let mut expression = self.parse_equality()?;
        while self.match_token(&TokenKind::AndAnd) {
            let right = self.parse_equality()?;
            expression = binary(expression, BinaryOperator::And, right);
        }
        Ok(expression)
    }

    fn parse_equality(&mut self) -> Result<Expression, ParseError> {
        let mut expression = self.parse_unary()?;
        loop {
            let operator = if self.match_token(&TokenKind::EqualEqual) {
                Some(BinaryOperator::Equal)
            } else if self.match_token(&TokenKind::BangEqual) {
                Some(BinaryOperator::NotEqual)
            } else {
                None
            };
            let Some(operator) = operator else { break };
            let right = self.parse_unary()?;
            expression = binary(expression, operator, right);
        }
        Ok(expression)
    }

    fn parse_unary(&mut self) -> Result<Expression, ParseError> {
        if self.match_token(&TokenKind::Bang) {
            let start = self.previous().span;
            let operand = self.parse_unary()?;
            Ok(Expression {
                span: start.join(operand.span),
                kind: ExpressionKind::Not(Box::new(operand)),
            })
        } else if self.match_token(&TokenKind::Ampersand) {
            let start = self.previous().span;
            let value = self.parse_unary()?;
            Ok(Expression {
                span: start.join(value.span),
                kind: ExpressionKind::Reference(Box::new(value)),
            })
        } else {
            self.parse_postfix()
        }
    }

    fn parse_postfix(&mut self) -> Result<Expression, ParseError> {
        let mut expression = self.parse_primary()?;
        loop {
            if self.match_token(&TokenKind::LeftParen) {
                let (arguments, end) = self.parse_arguments()?;
                expression = Expression {
                    span: expression.span.join(end),
                    kind: ExpressionKind::Call {
                        callee: Box::new(expression),
                        arguments,
                    },
                };
            } else if self.match_token(&TokenKind::LeftBracket) {
                let index = self.parse_expression()?;
                let end = self.consume(&TokenKind::RightBracket, "`]`")?.span;
                expression = Expression {
                    span: expression.span.join(end),
                    kind: ExpressionKind::Index {
                        object: Box::new(expression),
                        index: Box::new(index),
                    },
                };
            } else if self.match_token(&TokenKind::DoubleColon) {
                let (member, end) = self.consume_identifier("namespace member")?;
                expression = Expression {
                    span: expression.span.join(end),
                    kind: ExpressionKind::NamespaceAccess {
                        namespace: Box::new(expression),
                        member,
                    },
                };
            } else if self.match_token(&TokenKind::Dot) {
                let (member, member_span) = self.consume_identifier("member name")?;
                if self.match_token(&TokenKind::LeftParen) {
                    let (arguments, end) = self.parse_arguments()?;
                    expression = Expression {
                        span: expression.span.join(end),
                        kind: ExpressionKind::MethodCall {
                            receiver: Box::new(expression),
                            method: member,
                            arguments,
                        },
                    };
                } else {
                    expression = Expression {
                        span: expression.span.join(member_span),
                        kind: ExpressionKind::MemberAccess {
                            receiver: Box::new(expression),
                            member,
                        },
                    };
                }
            } else {
                break;
            }
        }
        Ok(expression)
    }

    fn parse_arguments(&mut self) -> Result<(Vec<Expression>, Span), ParseError> {
        let mut arguments = Vec::new();
        if !self.at(&TokenKind::RightParen) {
            loop {
                arguments.push(self.parse_expression()?);
                if !self.match_token(&TokenKind::Comma) {
                    break;
                }
            }
        }
        let end = self.consume(&TokenKind::RightParen, "`)`")?.span;
        Ok((arguments, end))
    }

    fn parse_primary(&mut self) -> Result<Expression, ParseError> {
        let token = self.advance();
        let kind = match token.kind {
            TokenKind::Identifier(value) => ExpressionKind::Identifier(value),
            TokenKind::Compiler => ExpressionKind::Identifier("compiler".into()),
            TokenKind::Self_ => ExpressionKind::Identifier("self".into()),
            TokenKind::Integer(value) => ExpressionKind::IntegerLiteral(value),
            TokenKind::String(value) => ExpressionKind::StringLiteral(value),
            TokenKind::True => ExpressionKind::BooleanLiteral(true),
            TokenKind::False => ExpressionKind::BooleanLiteral(false),
            TokenKind::LeftParen => {
                let mut expression = self.parse_expression()?;
                let end = self.consume(&TokenKind::RightParen, "`)`")?.span;
                expression.span = token.span.join(end);
                return Ok(expression);
            }
            TokenKind::LeftBracket => {
                let mut values = Vec::new();
                if !self.at(&TokenKind::RightBracket) {
                    loop {
                        values.push(self.parse_expression()?);
                        if !self.match_token(&TokenKind::Comma) {
                            break;
                        }
                    }
                }
                let end = self.consume(&TokenKind::RightBracket, "`]`")?.span;
                return Ok(Expression {
                    kind: ExpressionKind::Array(values),
                    span: token.span.join(end),
                });
            }
            actual => {
                return Err(ParseError {
                    expected: "expression".into(),
                    actual: actual.description(),
                    span: token.span,
                });
            }
        };
        Ok(Expression {
            kind,
            span: token.span,
        })
    }

    fn consume_identifier(&mut self, expected: &str) -> Result<(String, Span), ParseError> {
        let token = self.advance();
        match token.kind {
            TokenKind::Identifier(value) => Ok((value, token.span)),
            TokenKind::Compiler => Ok(("compiler".into(), token.span)),
            TokenKind::Self_ => Ok(("self".into(), token.span)),
            _ => Err(ParseError {
                expected: expected.into(),
                actual: token.kind.description(),
                span: token.span,
            }),
        }
    }

    fn consume(&mut self, kind: &TokenKind, expected: &str) -> Result<Token, ParseError> {
        if self.at(kind) {
            Ok(self.advance())
        } else {
            Err(self.error(expected))
        }
    }

    fn match_token(&mut self, kind: &TokenKind) -> bool {
        if self.at(kind) {
            self.current += 1;
            true
        } else {
            false
        }
    }

    fn at(&self, kind: &TokenKind) -> bool {
        std::mem::discriminant(&self.peek().kind) == std::mem::discriminant(kind)
    }

    fn advance(&mut self) -> Token {
        let token = self.peek().clone();
        if !self.at(&TokenKind::Eof) {
            self.current += 1;
        }
        token
    }

    fn previous(&self) -> &Token {
        &self.tokens[self.current - 1]
    }

    fn peek(&self) -> &Token {
        let final_index = self.tokens.len() - 1;
        &self.tokens[self.current.min(final_index)]
    }

    fn error(&self, expected: &str) -> ParseError {
        ParseError {
            expected: expected.into(),
            actual: self.peek().kind.description(),
            span: self.peek().span,
        }
    }
}

fn binary(left: Expression, operator: BinaryOperator, right: Expression) -> Expression {
    Expression {
        span: left.span.join(right.span),
        kind: ExpressionKind::Binary {
            left: Box::new(left),
            operator,
            right: Box::new(right),
        },
    }
}

#[cfg(test)]
mod tests {
    use crate::{Lexer, TopLevelItem};

    use super::*;

    fn program(source: &str) -> Program {
        Parser::new(Lexer::new(source).tokenize().unwrap())
            .parse_program()
            .unwrap()
    }

    #[test]
    fn parses_top_level_declarations_and_parameter() {
        let parsed = program("import \"gcc\"; #define SRC \"src\" section build(jobs: int) {}");
        assert_eq!(parsed.items.len(), 3);
        let TopLevelItem::Section(section) = &parsed.items[2] else {
            panic!("expected section");
        };
        assert_eq!(section.parameters[0].name, "jobs");
    }

    #[test]
    fn parses_all_statement_forms_and_postfix_expressions() {
        let parsed = program(
            r#"section build() {
                let cc = Compiler::new();
                if (gcc::get_compiler(&cc) != Compiler::AVAILABLE) {} else {}
                for-parallel file in dir.recursive(SRC) { parallel cc.compile(file); }
                cc.setflag("-c").outputs();
            }"#,
        );
        let TopLevelItem::Section(section) = &parsed.items[0] else {
            panic!("expected section");
        };
        assert_eq!(section.body.statements.len(), 4);
    }

    #[test]
    fn logical_and_binds_more_tightly_than_or() {
        let parsed = program("section x() { true || false && false; }");
        let TopLevelItem::Section(section) = &parsed.items[0] else {
            panic!("expected section");
        };
        let Statement::Expression { expression, .. } = &section.body.statements[0] else {
            panic!("expected expression statement");
        };
        let ExpressionKind::Binary {
            operator, right, ..
        } = &expression.kind
        else {
            panic!("expected binary expression");
        };
        assert_eq!(*operator, BinaryOperator::Or);
        assert!(matches!(
            right.kind,
            ExpressionKind::Binary {
                operator: BinaryOperator::And,
                ..
            }
        ));
    }

    #[test]
    fn diagnoses_missing_semicolon() {
        let tokens = Lexer::new("section x() { let value = 1 }")
            .tokenize()
            .unwrap();
        let error = Parser::new(tokens).parse_program().unwrap_err();
        assert_eq!(error.expected, "`;`");
        assert_eq!(error.actual, "`}`");
    }

    #[test]
    fn diagnoses_missing_closing_parenthesis() {
        let tokens = Lexer::new("section x( {}").tokenize().unwrap();
        let error = Parser::new(tokens).parse_program().unwrap_err();
        assert!(error.expected.contains("parameter"));
    }

    #[test]
    fn parses_complete_sample() {
        let parsed = program(
            r#"import "gcc"
import "clang"
#define SRC "./src"
#define BIN "myproject"
section build(jobs: int) {
 roller::set_parallel_jobs(jobs);
 let cc = Compiler::new();
 if (gcc::get_compiler(&cc) != Compiler::AVAILABLE)
     && (clang::get_compiler(&cc) != Compiler::AVAILABLE) {
   log::error("No compiler found");
   roller::exit(1);
 }
 let obj_compiler = cc.setflag("-c");
 for-parallel file in dir.recursive(SRC) {
   parallel obj_compiler.compile(file);
 }
 cc.link(obj_compiler.outputs(), BIN);
}"#,
        );
        assert_eq!(parsed.items.len(), 5);
    }

    #[test]
    fn parses_library_with_function() {
        let parsed =
            program(r#"library "test" { function greet(name: any) -> any { return name; } }"#);
        assert_eq!(parsed.items.len(), 1);
        let TopLevelItem::Library(lib) = &parsed.items[0] else {
            panic!("expected library")
        };
        assert_eq!(lib.name, "test");
        assert_eq!(lib.items.len(), 1);
        let LibraryItem::Function(func) = &lib.items[0] else {
            panic!("expected function")
        };
        assert_eq!(func.name, "greet");
        assert_eq!(func.parameters.len(), 1);
        assert_eq!(func.return_type, Some("any".into()));
    }

    #[test]
    fn parses_compiler_declaration_implementation_and_assignment() {
        let parsed = program(
            r#"library "test" {
                compiler cc { flags: Vec<String>, path: String }
                function select(compiler: Compiler) { compiler = self::cc; compiler.path = "cc"; }
                implement Self::cc {
                    paralleable function compile(compiler: self, file: String) { return compiler; }
                }
            }"#,
        );
        let TopLevelItem::Library(lib) = &parsed.items[0] else {
            panic!("expected library")
        };
        assert_eq!(lib.items.len(), 3);
        let LibraryItem::Compiler(compiler) = &lib.items[0] else {
            panic!("expected compiler declaration")
        };
        assert_eq!(compiler.name, "cc");
        assert_eq!(compiler.fields[0].type_name, "Vec<String>");
        let LibraryItem::Function(select) = &lib.items[1] else {
            panic!("expected selection function")
        };
        assert!(matches!(
            select.body.statements[0],
            Statement::Assignment { .. }
        ));
        assert!(matches!(
            select.body.statements[1],
            Statement::Assignment { .. }
        ));
        let LibraryItem::Implement(implementation) = &lib.items[2] else {
            panic!("expected implementation")
        };
        assert_eq!(implementation.compiler_name, "cc");
        assert!(implementation.methods[0].is_parallelable);
    }

    #[test]
    fn parses_not_operator() {
        let parsed = program("section x() { !true; }");
        let TopLevelItem::Section(s) = &parsed.items[0] else {
            panic!()
        };
        let Statement::Expression { expression, .. } = &s.body.statements[0] else {
            panic!()
        };
        assert!(matches!(expression.kind, ExpressionKind::Not(_)));
    }

    #[test]
    fn parses_array_index() {
        let parsed = program("section x() { let a = [1]; let b = a[0]; }");
        let TopLevelItem::Section(s) = &parsed.items[0] else {
            panic!()
        };
        let Statement::Let { value, .. } = &s.body.statements[1] else {
            panic!()
        };
        assert!(matches!(value.kind, ExpressionKind::Index { .. }));
    }

    #[test]
    fn parses_arrow_return_type() {
        let parsed = program(r#"library "x" { function f() -> int { return 1; } }"#);
        let TopLevelItem::Library(lib) = &parsed.items[0] else {
            panic!()
        };
        let LibraryItem::Function(func) = &lib.items[0] else {
            panic!()
        };
        assert_eq!(func.return_type, Some("int".into()));
    }
}
