//! Validated syntax nodes. Fields with structural invariants stay private.

use bpfman_shell_source::{Position, SourceName, Span};

use crate::token::VariableRef;

/// A parsed source unit with immutable statements and source identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedProgram {
    source: SourceName,
    statements: Vec<Statement>,
    span: Span,
}

impl ParsedProgram {
    pub(crate) fn new(source: SourceName, statements: Vec<Statement>, span: Span) -> Self {
        Self {
            source,
            statements,
            span,
        }
    }

    /// The source identity used by diagnostics.
    #[must_use]
    pub fn source(&self) -> &SourceName {
        &self.source
    }

    /// Statements in source order.
    #[must_use]
    pub fn statements(&self) -> &[Statement] {
        &self.statements
    }

    /// The half-open extent of the parsed source.
    #[must_use]
    pub fn span(&self) -> &Span {
        &self.span
    }
}

/// An ASCII binding identifier with its original token span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Identifier {
    text: String,
    span: Span,
}

impl Identifier {
    pub(crate) fn new(text: String, span: Span) -> Option<Self> {
        let mut bytes = text.bytes();
        if !bytes
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return None;
        }
        Some(Self { text, span })
    }

    /// Identifier spelling.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Source extent of the identifier.
    #[must_use]
    pub fn span(&self) -> &Span {
        &self.span
    }
}

/// An expression atom, with its source extent kept separate from its content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Expression {
    kind: ExpressionKind,
    span: Span,
}

impl Expression {
    pub(crate) fn new(kind: ExpressionKind, span: Span) -> Self {
        Self { kind, span }
    }

    /// The expression's typed content.
    #[must_use]
    pub fn kind(&self) -> &ExpressionKind {
        &self.kind
    }

    /// Source extent of the expression.
    #[must_use]
    pub fn span(&self) -> &Span {
        &self.span
    }
}

/// Operators with parser-checked precedence and spelling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BinaryOperator {
    /// `==`.
    Equal,
    /// `!=`.
    NotEqual,
    /// `<`.
    Less,
    /// `<=`.
    LessEqual,
    /// `>`.
    Greater,
    /// `>=`.
    GreaterEqual,
    /// `+`.
    Add,
    /// `-`.
    Subtract,
    /// `*`.
    Multiply,
    /// `/`.
    Divide,
    /// `%`.
    Remainder,
}

/// Short-circuit boolean operators.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LogicalOperator {
    /// Short-circuit conjunction.
    And,
    /// Short-circuit disjunction.
    Or,
}

/// Fixed-arity pure functions recognised by the syntax phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PureBuiltin {
    /// jq query over a value.
    Jq,
    /// Decode little-endian u32.
    U32Le,
    /// Decode little-endian u64.
    U64Le,
    /// Produce a numeric range.
    Range,
    /// Zip two lists.
    Zip,
    /// Check a path.
    PathExists,
    /// Check membership.
    Contains,
    /// Check whether a value is null.
    Null,
    /// Check presence.
    Present,
    /// Check absence.
    Missing,
    /// Check emptiness.
    Empty,
}

/// One part of an interpolated string; a segment has exactly one meaning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InterpolatedPart {
    /// Decoded literal text.
    Literal(String),
    /// Expression evaluated and rendered into the string.
    Expression(Box<Expression>),
}

/// Forms accepted by the incremental expression parser.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExpressionKind {
    /// Bare word, interpreted by the evaluator as a scalar candidate.
    Word(String),
    /// Quoted string, always a string value.
    Quoted(String),
    /// Variable and optional field/index path.
    Variable(VariableRef),
    /// Adapter name and its variable input.
    Adapter {
        /// Adapter name.
        name: String,
        /// Variable supplied to the adapter.
        variable: VariableRef,
    },
    /// Double-quoted text with embedded expressions.
    Interpolated(Vec<InterpolatedPart>),
    /// Arithmetic or comparison operation.
    Binary {
        /// Left operand.
        left: Box<Expression>,
        /// Checked operator.
        operator: BinaryOperator,
        /// Right operand.
        right: Box<Expression>,
    },
    /// Short-circuit boolean operation.
    Logical {
        /// Left operand.
        left: Box<Expression>,
        /// Checked operator.
        operator: LogicalOperator,
        /// Right operand.
        right: Box<Expression>,
    },
    /// Boolean negation.
    Not(Box<Expression>),
    /// Numeric negation.
    Negate(Box<Expression>),
    /// Nonempty predicate.
    NotEmpty(Box<Expression>),
    /// Thread a value into the final command argument.
    Thread {
        /// Value produced on the left.
        value: Box<Expression>,
        /// Command that receives the value.
        command: ThreadCall,
        /// Position of `|>` for diagnostics.
        pipe: Position,
    },
    /// Builtin whose argument count is checked during parsing.
    PureCall {
        /// Registered builtin.
        builtin: PureBuiltin,
        /// Arguments in source order.
        arguments: Vec<Expression>,
    },
    /// Whitespace-separated list literal.
    List(Vec<Expression>),
}

/// A nonempty command invocation. The head is evaluated before dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Command {
    head: Box<Expression>,
    arguments: Vec<Expression>,
    span: Span,
}

/// A nonempty threaded call. Its head may be a computed value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadCall {
    head: Box<Expression>,
    arguments: Vec<Expression>,
    span: Span,
}

impl ThreadCall {
    pub(crate) fn new(head: Expression, arguments: Vec<Expression>, span: Span) -> Self {
        Self {
            head: Box::new(head),
            arguments,
            span,
        }
    }

    /// Expression resolving to the command name.
    #[must_use]
    pub fn head(&self) -> &Expression {
        &self.head
    }

    /// Arguments before the threaded value.
    #[must_use]
    pub fn arguments(&self) -> &[Expression] {
        &self.arguments
    }

    /// Extent of the call following `|>`.
    #[must_use]
    pub fn span(&self) -> &Span {
        &self.span
    }
}

impl Command {
    pub(crate) fn new(head: Expression, arguments: Vec<Expression>, span: Span) -> Self {
        Self {
            head: Box::new(head),
            arguments,
            span,
        }
    }

    /// Expression resolving to the command name.
    #[must_use]
    pub fn head(&self) -> &Expression {
        &self.head
    }

    /// Arguments after the head, in source order.
    #[must_use]
    pub fn arguments(&self) -> &[Expression] {
        &self.arguments
    }

    /// Source extent of the complete command.
    #[must_use]
    pub fn span(&self) -> &Span {
        &self.span
    }
}

/// Binding mode for `<-` command results.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindMode {
    /// `let` retains the command outcome.
    Let,
    /// `guard` checks success and retains the declared value.
    Guard,
}

/// A parsed statement with its complete source extent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Statement {
    kind: StatementKind,
    span: Span,
}

impl Statement {
    pub(crate) fn new(kind: StatementKind, span: Span) -> Self {
        Self { kind, span }
    }

    /// The statement's typed content.
    #[must_use]
    pub fn kind(&self) -> &StatementKind {
        &self.kind
    }

    /// Source extent of the statement.
    #[must_use]
    pub fn span(&self) -> &Span {
        &self.span
    }
}

/// One conditional branch and its lexical body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IfBranch {
    condition: Expression,
    body: Vec<Statement>,
    span: Span,
}

impl IfBranch {
    pub(crate) fn new(condition: Expression, body: Vec<Statement>, span: Span) -> Self {
        Self {
            condition,
            body,
            span,
        }
    }

    /// Condition evaluated when this branch is reached.
    #[must_use]
    pub fn condition(&self) -> &Expression {
        &self.condition
    }

    /// Statements executed when the condition is true.
    #[must_use]
    pub fn body(&self) -> &[Statement] {
        &self.body
    }

    /// Extent from the branch keyword through its closing brace.
    #[must_use]
    pub fn span(&self) -> &Span {
        &self.span
    }
}

/// Validated loop bindings. A destructured list has at least two names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoopBindings {
    names: Vec<Identifier>,
    destructured: bool,
}

/// A parameter type checked when a definition is called.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParameterType {
    /// JSON number.
    Number,
    /// Text string.
    String,
    /// Boolean value.
    Bool,
}

/// One validated definition parameter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Parameter {
    name: Identifier,
    annotation: Option<ParameterType>,
}

impl Parameter {
    pub(crate) fn new(name: Identifier, annotation: Option<ParameterType>) -> Self {
        Self { name, annotation }
    }

    /// Binding name.
    #[must_use]
    pub fn name(&self) -> &Identifier {
        &self.name
    }

    /// Optional type annotation.
    #[must_use]
    pub fn annotation(&self) -> Option<ParameterType> {
        self.annotation
    }
}

/// A user-defined command with an eagerly parsed body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Definition {
    name: Identifier,
    parameters: Vec<Parameter>,
    body: Vec<Statement>,
}

impl Definition {
    pub(crate) fn new(name: Identifier, parameters: Vec<Parameter>, body: Vec<Statement>) -> Self {
        Self {
            name,
            parameters,
            body,
        }
    }

    /// Name used for command dispatch.
    #[must_use]
    pub fn name(&self) -> &Identifier {
        &self.name
    }

    /// Parameters in declaration order.
    #[must_use]
    pub fn parameters(&self) -> &[Parameter] {
        &self.parameters
    }

    /// Parsed body statements.
    #[must_use]
    pub fn body(&self) -> &[Statement] {
        &self.body
    }
}

impl LoopBindings {
    pub(crate) fn single(name: Identifier) -> Self {
        Self {
            names: vec![name],
            destructured: false,
        }
    }

    pub(crate) fn destructured(names: Vec<Identifier>) -> Option<Self> {
        if names.len() < 2 || names.iter().all(|name| name.text() == "_") {
            return None;
        }
        Some(Self {
            names,
            destructured: true,
        })
    }

    /// Binding names in source order; `_` is an explicit discarded slot.
    #[must_use]
    pub fn names(&self) -> &[Identifier] {
        &self.names
    }

    /// Whether the names were written in parentheses.
    #[must_use]
    pub fn is_destructured(&self) -> bool {
        self.destructured
    }
}

/// Statement forms accepted by this parser slice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StatementKind {
    /// Plain command invocation.
    Command(Command),
    /// Evaluate an expression in statement position and discard its value.
    Expression(Expression),
    /// User-defined command declaration.
    Definition(Definition),
    /// Conditional chain with an optional final else body.
    If {
        /// Primary condition and body.
        first: IfBranch,
        /// Additional branches in source order.
        elif: Vec<IfBranch>,
        /// Present even when an explicit else body is empty.
        else_body: Option<Vec<Statement>>,
    },
    /// Iterate over an expression, binding its elements in the body.
    Foreach {
        /// One name or a validated destructuring list.
        bindings: LoopBindings,
        /// Expression producing the list.
        list: Expression,
        /// Statements executed for each element.
        body: Vec<Statement>,
    },
    /// `let NAME = EXPR`.
    Let {
        /// Bound name.
        name: Identifier,
        /// Value expression.
        value: Expression,
    },
    /// `let NAME <- COMMAND` or `guard NAME <- COMMAND`.
    Bind {
        /// Outcome or guard binding.
        mode: BindMode,
        /// Bound name.
        name: Identifier,
        /// Command producing the binding.
        command: Command,
    },
    /// Cleanup command registered for the enclosing scope.
    Defer(Command),
    /// Return expression from a definition.
    Return(Expression),
    /// Break the nearest foreach loop.
    Break,
    /// Continue the nearest foreach loop.
    Continue,
}
