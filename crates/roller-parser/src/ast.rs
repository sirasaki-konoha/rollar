//! Abstract syntax tree definitions.

/// One source position. Lines and columns are one-based; offsets are bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Position {
    /// Byte offset from the start of the source.
    pub offset: usize,
    /// One-based line number.
    pub line: usize,
    /// One-based UTF-8 character column.
    pub column: usize,
}

/// Half-open source range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    /// Inclusive start position.
    pub start: Position,
    /// Exclusive end position.
    pub end: Position,
}

impl Span {
    /// Create a range spanning both inputs.
    #[must_use]
    pub fn join(self, other: Self) -> Self {
        Self {
            start: self.start,
            end: other.end,
        }
    }
}

/// Complete Roller source file.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    /// Top-level declarations in source order.
    pub items: Vec<TopLevelItem>,
    /// Source range covering the program.
    pub span: Span,
}

/// A declaration allowed at file scope.
#[derive(Debug, Clone, PartialEq)]
pub enum TopLevelItem {
    /// Module import.
    Import(ImportDeclaration),
    /// Roller constant declaration.
    Constant(ConstantDeclaration),
    /// Callable section declaration.
    Section(SectionDeclaration),
    /// Inline library definition.
    Library(LibraryDeclaration),
}

impl TopLevelItem {
    /// Source range of this declaration.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Import(value) => value.span,
            Self::Constant(value) => value.span,
            Self::Section(value) => value.span,
            Self::Library(value) => value.span,
        }
    }
}

/// `import "name"` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportDeclaration {
    /// Imported module name.
    pub module: String,
    /// Source range.
    pub span: Span,
}

/// `#define NAME expression` declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct ConstantDeclaration {
    /// Constant identifier.
    pub name: String,
    /// Constant value expression.
    pub value: Expression,
    /// Source range.
    pub span: Span,
}

/// A named CLI-callable section.
#[derive(Debug, Clone, PartialEq)]
pub struct SectionDeclaration {
    /// Section name.
    pub name: String,
    /// Declared parameters.
    pub parameters: Vec<Parameter>,
    /// Section body.
    pub body: Block,
    /// Source range.
    pub span: Span,
}

/// A section parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    /// Parameter name.
    pub name: String,
    /// Written type name.
    pub type_name: String,
    /// Source range.
    pub span: Span,
}

/// Inline library definition.
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryDeclaration {
    /// Library name.
    pub name: String,
    /// Library items (functions and override blocks).
    pub items: Vec<LibraryItem>,
    /// Source range.
    pub span: Span,
}

/// An item inside a library block.
#[derive(Debug, Clone, PartialEq)]
pub enum LibraryItem {
    /// Function definition.
    Function(FunctionDeclaration),
    /// Concrete implementation of the core `Compiler` contract.
    Compiler(CompilerDeclaration),
    /// Methods implemented by a concrete compiler.
    Implement(ImplementBlock),
}

/// A concrete compiler declaration inside a library.
#[derive(Debug, Clone, PartialEq)]
pub struct CompilerDeclaration {
    /// Implementation-local name.
    pub name: String,
    /// Runtime fields stored by this compiler implementation.
    pub fields: Vec<FieldDeclaration>,
    /// Source range.
    pub span: Span,
}

/// A field in a concrete compiler declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDeclaration {
    /// Field name.
    pub name: String,
    /// Written type, including an optional generic argument.
    pub type_name: String,
    /// Source range.
    pub span: Span,
}

/// An `implement Self::<compiler>` method block.
#[derive(Debug, Clone, PartialEq)]
pub struct ImplementBlock {
    /// Concrete compiler name in the containing library.
    pub compiler_name: String,
    /// Methods implemented for the compiler.
    pub methods: Vec<FunctionDeclaration>,
    /// Source range.
    pub span: Span,
}

/// A function declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionDeclaration {
    /// Function name.
    pub name: String,
    /// Declared parameters.
    pub parameters: Vec<Parameter>,
    /// Optional return type.
    pub return_type: Option<String>,
    /// Function body.
    pub body: Block,
    /// Whether this is a parallelizable function.
    pub is_parallelable: bool,
    /// Source range.
    pub span: Span,
}

/// Lexical block.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    /// Statements in source order.
    pub statements: Vec<Statement>,
    /// Source range including braces.
    pub span: Span,
}

/// Executable statement.
#[derive(Debug, Clone, PartialEq)]
pub enum Statement {
    /// Local binding.
    Let {
        /// Binding name.
        name: String,
        /// Initial value.
        value: Expression,
        /// Source range.
        span: Span,
    },
    /// Assignment to a local binding or compiler field.
    Assignment {
        /// Assignment destination.
        target: Expression,
        /// New value.
        value: Expression,
        /// Source range.
        span: Span,
    },
    /// Conditional execution.
    If {
        /// Condition expression.
        condition: Expression,
        /// True branch.
        then_block: Block,
        /// Optional false branch.
        else_block: Option<Block>,
        /// Source range.
        span: Span,
    },
    /// Parallel iteration.
    ForParallel {
        /// Per-iteration binding.
        binding: String,
        /// Iterable expression.
        iterable: Expression,
        /// Iteration body.
        body: Block,
        /// Source range.
        span: Span,
    },
    /// Scheduled expression.
    Parallel {
        /// Expression to schedule.
        expression: Expression,
        /// Source range.
        span: Span,
    },
    /// Expression evaluated for side effects.
    Expression {
        /// Expression to evaluate.
        expression: Expression,
        /// Source range.
        span: Span,
    },
    /// Early return with value.
    Return {
        /// Return value.
        value: Expression,
        /// Source range.
        span: Span,
    },
}

impl Statement {
    /// Source range of the statement.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Let { span, .. }
            | Self::Assignment { span, .. }
            | Self::If { span, .. }
            | Self::ForParallel { span, .. }
            | Self::Parallel { span, .. }
            | Self::Expression { span, .. }
            | Self::Return { span, .. } => *span,
        }
    }
}

/// Expression and its source range.
#[derive(Debug, Clone, PartialEq)]
pub struct Expression {
    /// Expression form.
    pub kind: ExpressionKind,
    /// Source range.
    pub span: Span,
}

/// Expression form.
#[derive(Debug, Clone, PartialEq)]
pub enum ExpressionKind {
    /// Variable or constant identifier.
    Identifier(String),
    /// Unsigned integer literal.
    IntegerLiteral(u64),
    /// String literal.
    StringLiteral(String),
    /// Boolean literal.
    BooleanLiteral(bool),
    /// Function or callable-value invocation.
    Call {
        /// Invoked expression.
        callee: Box<Expression>,
        /// Call arguments.
        arguments: Vec<Expression>,
    },
    /// Method invocation.
    MethodCall {
        /// Receiver expression.
        receiver: Box<Expression>,
        /// Method name.
        method: String,
        /// Call arguments.
        arguments: Vec<Expression>,
    },
    /// Namespace-qualified name.
    NamespaceAccess {
        /// Namespace expression.
        namespace: Box<Expression>,
        /// Qualified member.
        member: String,
    },
    /// Object member access.
    MemberAccess {
        /// Receiver expression.
        receiver: Box<Expression>,
        /// Member name.
        member: String,
    },
    /// Mutable runtime-object reference.
    Reference(Box<Expression>),
    /// Binary operator expression.
    Binary {
        /// Left operand.
        left: Box<Expression>,
        /// Operator.
        operator: BinaryOperator,
        /// Right operand.
        right: Box<Expression>,
    },
    /// Array literal.
    Array(Vec<Expression>),
    /// Logical NOT (prefix `!`).
    Not(Box<Expression>),
    /// Array/object indexing (`expr[expr]`).
    Index {
        /// Object being indexed.
        object: Box<Expression>,
        /// Index expression.
        index: Box<Expression>,
    },
}

/// Supported binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOperator {
    /// Equality.
    Equal,
    /// Inequality.
    NotEqual,
    /// Short-circuit logical and.
    And,
    /// Short-circuit logical or.
    Or,
}
