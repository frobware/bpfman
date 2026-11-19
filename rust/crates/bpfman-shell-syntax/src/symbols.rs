//! Pure extraction of bindings and their editor visibility scopes.

use bpfman_shell_source::{Position, Span};

use crate::{Identifier, LoopBindings, ParsedProgram, Statement, StatementKind};

/// The syntax that introduced one named binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymbolKind {
    /// User-defined command.
    Definition,
    /// User-defined command parameter.
    Parameter,
    /// Expression binding.
    Let,
    /// One slot in a destructuring binding.
    Destructure,
    /// Command or collection result binding.
    Bind,
    /// Iteration variable.
    Foreach,
}

impl SymbolKind {
    /// The editor protocol's spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Definition => "def",
            Self::Parameter => "param",
            Self::Let => "let",
            Self::Destructure => "destructure",
            Self::Bind => "bind",
            Self::Foreach => "foreach",
        }
    }
}

/// A source binding and its syntax-level visibility.
#[derive(Debug, Eq, PartialEq)]
pub struct Symbol {
    /// The binding's name.
    pub name: String,
    /// Syntax introducing the binding.
    pub kind: SymbolKind,
    /// Start of the identifier token.
    pub definition: Position,
    /// Visibility scope reported to editor tooling.
    pub scope: Span,
}

/// The root editor scope, from the first byte to the last statement.
#[must_use]
pub fn program_scope(program: &ParsedProgram) -> Span {
    let start = Position::origin(program.source().clone());
    let end = program
        .statements()
        .last()
        .map_or_else(|| start.clone(), |statement| statement.span().end().clone());
    Span::new(start, end).unwrap_or_else(|_| program.span().clone())
}

/// Extract local bindings and declarations in source order.
#[must_use]
pub fn symbols(program: &ParsedProgram) -> Vec<Symbol> {
    let mut output = Vec::new();
    walk(program.statements(), &program_scope(program), &mut output);
    output
}

fn push(output: &mut Vec<Symbol>, identifier: &Identifier, kind: SymbolKind, scope: &Span) {
    if identifier.text() != "_" {
        output.push(Symbol {
            name: identifier.text().to_owned(),
            kind,
            definition: identifier.span().start().clone(),
            scope: scope.clone(),
        });
    }
}

fn walk(statements: &[Statement], scope: &Span, output: &mut Vec<Symbol>) {
    for statement in statements {
        match statement.kind() {
            StatementKind::Let { name, .. } => push(output, name, SymbolKind::Let, scope),
            StatementKind::LetDestructure { names, .. } => {
                for name in names {
                    push(output, name, SymbolKind::Destructure, scope);
                }
            }
            StatementKind::Bind { name, .. } => push(output, name, SymbolKind::Bind, scope),
            StatementKind::BindCollect {
                name,
                bindings,
                body,
                foreach_span,
                ..
            } => {
                push(output, name, SymbolKind::Bind, scope);
                foreach(bindings, body, foreach_span, output);
            }
            StatementKind::If {
                first,
                elif,
                else_body,
            } => {
                walk(first.body(), scope, output);
                for branch in elif {
                    walk(branch.body(), scope, output);
                }
                if let Some(body) = else_body {
                    walk(body, scope, output);
                }
            }
            StatementKind::Foreach { bindings, body, .. } => {
                foreach(bindings, body, statement.span(), output)
            }
            StatementKind::Poll { body, .. } => walk(body, scope, output),
            StatementKind::Definition(definition) => {
                push(output, definition.name(), SymbolKind::Definition, scope);
                for parameter in definition.parameters() {
                    push(
                        output,
                        parameter.name(),
                        SymbolKind::Parameter,
                        statement.span(),
                    );
                }
                walk(definition.body(), statement.span(), output);
            }
            _ => {}
        }
    }
}

fn foreach(bindings: &LoopBindings, body: &[Statement], scope: &Span, output: &mut Vec<Symbol>) {
    for name in bindings.names() {
        push(output, name, SymbolKind::Foreach, scope);
    }
    walk(body, scope, output);
}
