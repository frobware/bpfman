//! Validated syntax nodes. Fields with structural invariants stay private.

use bpfman_shell_source::{SourceName, Span};

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

/// Forms currently accepted by the incremental expression parser.
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
}

/// A lexical word naming a command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandHead {
    text: String,
    span: Span,
}

impl CommandHead {
    pub(crate) fn new(text: String, span: Span) -> Option<Self> {
        if text.is_empty() || matches!(text.as_str(), "{" | "}" | "(" | ")" | "[" | "]") {
            return None;
        }
        Some(Self { text, span })
    }

    /// Command name exactly as written.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Source extent of the command name.
    #[must_use]
    pub fn span(&self) -> &Span {
        &self.span
    }
}

/// A command invocation with a mandatory word head.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Command {
    head: CommandHead,
    arguments: Vec<Expression>,
    span: Span,
}

impl Command {
    pub(crate) fn new(head: CommandHead, arguments: Vec<Expression>, span: Span) -> Self {
        Self {
            head,
            arguments,
            span,
        }
    }

    /// The word naming the command.
    #[must_use]
    pub fn head(&self) -> &CommandHead {
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

/// Statement forms accepted by this parser slice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StatementKind {
    /// Plain command invocation.
    Command(Command),
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
