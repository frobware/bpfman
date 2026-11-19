use bpfman_shell_source::{Position, Span};

use crate::{Command, Located};

/// A lowered expression with source coordinates.
pub type Expression = Located<ExpressionKind>;

/// Lowered expression operands.
#[derive(Debug, Eq, PartialEq)]
pub enum ExpressionKind {
    /// An unquoted word, classified by the evaluator.
    Word(String),
    /// A quoted string.
    Quoted(String),
    /// A variable lookup with optional path.
    Variable(VariableRef),
    /// An adapter applied to a variable lookup.
    Adapter {
        /// Adapter name.
        name: String,
        /// Input variable.
        variable: VariableRef,
    },
    /// Text and evaluated fragments in source order.
    Interpolated(Vec<InterpolatedPart>),
    /// Named fields evaluated in source order.
    Record(Vec<RecordField>),
    /// Apply a predicate or unary operator.
    Unary {
        /// Operator.
        operator: UnaryOperator,
        /// Operand.
        operand: Box<Expression>,
    },
    /// A short-circuit logical operation.
    Logical {
        /// Left operand.
        left: Box<Expression>,
        /// Operator.
        operator: LogicalOperator,
        /// Right operand evaluated if required.
        right: Box<Expression>,
    },
    /// Evaluate a builtin with its ordered expression arguments.
    PureCall {
        /// Builtin identity.
        builtin: PureBuiltin,
        /// Arguments in evaluation order.
        arguments: Vec<Expression>,
    },
    /// Pass the left-hand value to a command in expression position.
    Thread {
        /// Left-hand value.
        value: Box<Expression>,
        /// Command receiving the value.
        command: Box<Command>,
        /// Location of the pipe operator.
        pipe: Position,
    },
    /// Match the selected fields of a value.
    Matches {
        /// Value being inspected.
        target: Box<Expression>,
        /// Ordered pattern block.
        block: MatchBlock,
    },
    /// A list evaluated left to right.
    List(Vec<Expression>),
    /// Apply an arithmetic or comparison operator.
    Binary {
        /// Left operand.
        left: Box<Expression>,
        /// Operator.
        operator: BinaryOperator,
        /// Right operand.
        right: Box<Expression>,
    },
}

/// Arithmetic and comparison operators understood by the IR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinaryOperator {
    /// Addition.
    Add,
    /// Subtraction.
    Subtract,
    /// Multiplication.
    Multiply,
    /// Division.
    Divide,
    /// Remainder.
    Remainder,
    /// Equality.
    Equal,
    /// Inequality.
    NotEqual,
    /// Less than.
    Less,
    /// Less than or equal.
    LessEqual,
    /// Greater than.
    Greater,
    /// Greater than or equal.
    GreaterEqual,
}

/// A variable lookup with an optional field or index path.
#[derive(Debug, Eq, PartialEq)]
pub struct VariableRef {
    /// Lexical name.
    pub name: String,
    /// Path without an initial dot.
    pub path: String,
}

/// One fragment of an interpolated string.
#[derive(Debug, Eq, PartialEq)]
pub enum InterpolatedPart {
    /// Literal text.
    Literal(String),
    /// Expression whose rendered value is appended.
    Expression(Expression),
}

/// A named field in a record literal.
#[derive(Debug, Eq, PartialEq)]
pub struct RecordField {
    /// Field name.
    pub name: String,
    /// Field value.
    pub value: Expression,
    /// Extent of the field.
    pub span: Span,
}

/// A structural match pattern.
#[derive(Debug, Eq, PartialEq)]
pub enum MatchPattern {
    /// Compare against an expression.
    Value(Expression),
    /// Apply a predicate.
    Predicate(MatchPredicate),
    /// Match a nested structure.
    Nested(MatchBlock),
}

/// A lowered row of a structural match.
#[derive(Debug, Eq, PartialEq)]
pub struct MatchEntry {
    /// Field or index path.
    pub path: String,
    /// Pattern for the selected value.
    pub pattern: MatchPattern,
    /// Extent of the row.
    pub span: Span,
}

/// An ordered structural match block.
#[derive(Debug, Eq, PartialEq)]
pub struct MatchBlock {
    /// Rows in source order.
    pub entries: Vec<MatchEntry>,
    /// Whether to reject extra fields.
    pub exhaustive: bool,
    /// Extent of the block.
    pub span: Span,
}

/// Unary operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnaryOperator {
    /// Boolean negation.
    Not,
    /// Numeric negation.
    Negate,
    /// Nonempty check.
    NotEmpty,
}

/// Short-circuit logical operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogicalOperator {
    /// Logical conjunction.
    And,
    /// Logical disjunction.
    Or,
}

/// Bare predicates in match rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchPredicate {
    /// Match nonempty values.
    NotEmpty,
    /// Match null.
    Null,
    /// Match empty values.
    Empty,
}

/// Builtin identities in expression position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PureBuiltin {
    /// The `jq` builtin.
    Jq,
    /// The `u32le` builtin.
    U32Le,
    /// The `u64le` builtin.
    U64Le,
    /// The `range` builtin.
    Range,
    /// The `zip` builtin.
    Zip,
    /// The `path-exists` builtin.
    PathExists,
    /// The `contains` builtin.
    Contains,
    /// The `null` builtin.
    Null,
    /// The `present` builtin.
    Present,
    /// The `missing` builtin.
    Missing,
    /// The `empty` builtin.
    Empty,
}
