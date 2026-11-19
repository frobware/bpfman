//! Canonical, source-preserving formatting of a parsed shell file.

use std::{fmt, fmt::Write as _, ops::RangeInclusive};

use bpfman_shell_source::Span;

use crate::{
    AssertionClause, AssertionMode, BindMode, Command, CommandOutcome, Expression, Identifier,
    IfBranch, LoopBindings, ParameterType, ParsedProgram, Statement, StatementKind, SyntaxError,
    dump::semantic_shape, parse, spelling::duration, token::strip_comments,
};

use self::expression::Position;

mod expression;

/// A formatter output could not be verified against the parsed input.
#[derive(Debug)]
pub enum FormatError {
    /// The formatter produced invalid source.
    InvalidOutput(SyntaxError),
    /// Formatting changed the structure of the program.
    ChangedSyntax,
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOutput(error) => write!(f, "invalid formatted source: {error}"),
            Self::ChangedSyntax => {
                f.write_str("internal formatter error: formatted source changed syntax")
            }
        }
    }
}

impl std::error::Error for FormatError {}

/// Render canonical source with original comments and command line groups.
/// The parsed program must belong to the given source.
pub fn format_source(source: &str, program: &ParsedProgram) -> Result<String, FormatError> {
    let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
    let mut formatter = Formatter {
        code: String::new(),
        lines: normalized.split_terminator('\n').collect(),
    };
    formatter.statements(program.statements(), 0, Some(1..=formatter.lines.len()));
    let written =
        parse(program.source().clone(), &formatter.code).map_err(FormatError::InvalidOutput)?;
    if semantic_shape(program.statements()) != semantic_shape(written.statements()) {
        return Err(FormatError::ChangedSyntax);
    }
    Ok(formatter.code)
}

struct Formatter<'a> {
    code: String,
    lines: Vec<&'a str>,
}

impl Formatter<'_> {
    fn statements(
        &mut self,
        statements: &[Statement],
        depth: usize,
        source: Option<RangeInclusive<usize>>,
    ) {
        let mut next = source.as_ref().map_or(0, |range| *range.start());
        for statement in statements {
            let span = statement.span();
            let start = span.start().line();
            let end = span.end().line();
            let aware = source.is_some();
            let continued = aware && self.continued(start, end);
            if aware {
                self.trivia(next, start.saturating_sub(1));
                if !block(statement.kind()) && !continued {
                    self.interior_comments(start + 1, end.saturating_sub(1), depth);
                }
            }
            indent(&mut self.code, depth);
            self.statement(statement, depth, aware, continued);
            if aware {
                self.trailing_comment(end);
                next = end + 1;
            }
            self.code.push('\n');
        }
        if let Some(range) = source {
            self.trivia(next, *range.end());
        }
    }

    fn statement(&mut self, statement: &Statement, depth: usize, aware: bool, continued: bool) {
        let span = statement.span();
        let start = span.start().line();
        let end = span.end().line();
        let window = (aware && start < end).then_some(start..=end.saturating_sub(1));
        match statement.kind() {
            StatementKind::Let { name, value } => {
                let _ = write!(self.code, "let {} = ", name.text());
                self.expr(value, Some(depth));
            }
            StatementKind::LetDestructure { names, value } => {
                let _ = write!(self.code, "let ({}) = ", identifiers(names));
                self.expr(value, Some(depth));
            }
            StatementKind::Bind {
                mode,
                name,
                command,
            } => {
                let _ = write!(self.code, "{} {} <- ", bind(*mode), name.text());
                self.command(command, depth, continued);
            }
            StatementKind::BindCollect {
                mode,
                name,
                bindings,
                list,
                body,
                foreach_span,
            } => {
                let _ = write!(self.code, "{} {} <- ", bind(*mode), name.text());
                self.foreach(
                    bindings,
                    list,
                    body,
                    depth,
                    if aware {
                        body_range(foreach_span)
                    } else {
                        None
                    },
                );
            }
            StatementKind::Command(command) => self.command(command, depth, continued),
            StatementKind::Defer(command) => {
                self.code.push_str("defer ");
                self.command(command, depth, continued);
            }
            StatementKind::Expression(expression) => {
                self.code.push('(');
                self.expr(expression, None);
                self.code.push(')');
            }
            StatementKind::Definition(definition) => {
                let _ = write!(self.code, "def {}(", definition.name().text());
                for (index, parameter) in definition.parameters().iter().enumerate() {
                    if index != 0 {
                        self.code.push(' ');
                    }
                    self.code.push_str(parameter.name().text());
                    if let Some(annotation) = parameter.annotation() {
                        self.code.push_str(": ");
                        self.code.push_str(match annotation {
                            ParameterType::Number => "number",
                            ParameterType::String => "string",
                            ParameterType::Bool => "bool",
                        });
                    }
                }
                self.code.push(')');
                self.body(definition.body(), depth, window);
            }
            StatementKind::If {
                first,
                elif,
                else_body,
            } => {
                self.conditional(first, elif, else_body.as_deref(), depth, window);
            }
            StatementKind::Foreach {
                bindings,
                list,
                body,
            } => self.foreach(bindings, list, body, depth, window),
            StatementKind::Poll {
                timeout,
                every,
                body,
            } => {
                let _ = write!(
                    self.code,
                    "poll timeout {} every {}",
                    duration(*timeout),
                    duration(*every)
                );
                self.body(body, depth, window);
            }
            StatementKind::Retry { message, unless } => {
                self.code.push_str("retry");
                if let Some(message) = message {
                    self.code.push(' ');
                    self.expr(message, None);
                }
                if let Some(unless) = unless {
                    self.code.push_str(" unless ");
                    self.expr(unless, None);
                }
            }
            StatementKind::Return(expression) => {
                self.code.push_str("return ");
                self.expr(expression, Some(depth));
            }
            StatementKind::Break => self.code.push_str("break"),
            StatementKind::Continue => self.code.push_str("continue"),
            StatementKind::Assertion { mode, clause } => {
                self.code.push_str(if *mode == AssertionMode::Assert {
                    "assert "
                } else {
                    "require "
                });
                match clause {
                    AssertionClause::Expression(expression) => self.expr(expression, Some(depth)),
                    AssertionClause::CommandStatus {
                        outcome,
                        arguments,
                        negated,
                        ..
                    } => {
                        if *negated {
                            self.code.push_str("not ");
                        }
                        self.code.push_str(if *outcome == CommandOutcome::Ok {
                            "ok"
                        } else {
                            "fail"
                        });
                        if !arguments.is_empty() {
                            self.code.push(' ');
                            self.argv(arguments.iter(), depth, continued);
                        }
                    }
                }
            }
        }
    }

    fn conditional(
        &mut self,
        first: &IfBranch,
        elif: &[IfBranch],
        elsewhere: Option<&[Statement]>,
        depth: usize,
        source: Option<RangeInclusive<usize>>,
    ) {
        let elsewhere = elsewhere.filter(|body| !body.is_empty());
        let else_line = elsewhere
            .and_then(|body| body.first())
            .map(|statement| statement.span().start().line().saturating_sub(1));
        self.code.push_str("if ");
        self.expr(first.condition(), None);
        let first_window = source.as_ref().map(|window| {
            let next = elif
                .first()
                .map(|branch| branch.span().start().line())
                .or(else_line);
            *window.start()..=next.map_or(*window.end(), |line| line.saturating_sub(1))
        });
        self.body(first.body(), depth, first_window);
        for (index, branch) in elif.iter().enumerate() {
            self.code.push_str(" elif ");
            self.expr(branch.condition(), None);
            let branch_window = source.as_ref().map(|_| {
                let next = elif
                    .get(index + 1)
                    .map(|branch| branch.span().start().line())
                    .or(else_line)
                    .unwrap_or_else(|| branch.span().end().line());
                branch.span().start().line()..=next.saturating_sub(1)
            });
            self.body(branch.body(), depth, branch_window);
        }
        if let Some(elsewhere) = elsewhere {
            self.code.push_str(" else");
            let else_window = source.and_then(|window| else_line.map(|line| line..=*window.end()));
            self.body(elsewhere, depth, else_window);
        }
    }

    fn foreach(
        &mut self,
        bindings: &LoopBindings,
        list: &Expression,
        body: &[Statement],
        depth: usize,
        source: Option<RangeInclusive<usize>>,
    ) {
        self.code.push_str("foreach ");
        if bindings.is_destructured() {
            self.code.push('(');
        }
        self.code.push_str(&identifiers(bindings.names()));
        if bindings.is_destructured() {
            self.code.push(')');
        }
        self.code.push_str(" in ");
        self.expr(list, None);
        self.body(body, depth, source);
    }

    fn body(&mut self, body: &[Statement], depth: usize, source: Option<RangeInclusive<usize>>) {
        self.code.push_str(" {");
        let body_range = source.map(|window| {
            self.trailing_comment(*window.start());
            (*window.start() + 1)..=*window.end()
        });
        self.code.push('\n');
        self.statements(body, depth + 1, body_range);
        indent(&mut self.code, depth);
        self.code.push('}');
    }

    fn expr(&mut self, expression: &Expression, depth: Option<usize>) {
        expression::write(&mut self.code, expression, Position::Top, depth);
    }

    fn command(&mut self, command: &Command, depth: usize, continued: bool) {
        self.argv(
            std::iter::once(command.head()).chain(command.arguments()),
            depth,
            continued,
        );
    }

    fn argv<'a>(
        &mut self,
        args: impl Iterator<Item = &'a Expression>,
        depth: usize,
        continued: bool,
    ) {
        let mut previous = None;
        for argument in args {
            let line = argument.span().start().line();
            if let Some(previous) = previous {
                if continued && previous != line {
                    self.code.push(' ');
                    self.code.push('\\');
                    self.code.push('\n');
                    indent(&mut self.code, depth + 1);
                } else {
                    self.code.push(' ');
                }
            }
            expression::write(&mut self.code, argument, Position::CommandArgument, None);
            previous = Some(line);
        }
    }

    fn trivia(&mut self, start: usize, end: usize) {
        for index in start.max(1)..=end.min(self.lines.len()) {
            let line = self.lines[index - 1];
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                self.code.push_str(line);
                self.code.push('\n');
            }
        }
    }

    fn interior_comments(&mut self, start: usize, end: usize, depth: usize) {
        for index in start.max(1)..=end.min(self.lines.len()) {
            if let Some(comment) = line_comment(self.lines[index - 1]) {
                indent(&mut self.code, depth);
                self.code.push_str(comment);
                self.code.push('\n');
            }
        }
    }

    fn trailing_comment(&mut self, line: usize) {
        if let Some(line) = line
            .checked_sub(1)
            .and_then(|offset| self.lines.get(offset))
            && !line.trim_start().starts_with('#')
            && let Some(comment) = line_comment(line)
        {
            self.code.push(' ');
            self.code.push_str(comment);
        }
    }

    fn continued(&self, start: usize, end: usize) -> bool {
        (start.max(1)..end.min(self.lines.len())).any(|line| {
            self.lines[line - 1]
                .trim_end_matches([' ', '\t'])
                .ends_with('\\')
        })
    }
}

fn body_range(span: &Span) -> Option<RangeInclusive<usize>> {
    let start = span.start().line();
    let end = span.end().line();
    (start < end).then_some(start..=end.saturating_sub(1))
}

fn line_comment(line: &str) -> Option<&str> {
    let stripped = strip_comments(line);
    let comment = line
        .bytes()
        .zip(stripped.bytes())
        .position(|(original, stripped)| original == b'#' && stripped == b' ')?;
    Some(line[comment..].trim())
}

fn indent(code: &mut String, depth: usize) {
    for _ in 0..depth {
        code.push_str("    ");
    }
}

fn block(kind: &StatementKind) -> bool {
    matches!(
        kind,
        StatementKind::If { .. }
            | StatementKind::Foreach { .. }
            | StatementKind::Poll { .. }
            | StatementKind::Definition(_)
            | StatementKind::BindCollect { .. }
    )
}

fn bind(mode: BindMode) -> &'static str {
    match mode {
        BindMode::Let => "let",
        BindMode::Guard => "guard",
    }
}

fn identifiers(identifiers: &[Identifier]) -> String {
    identifiers
        .iter()
        .map(Identifier::text)
        .collect::<Vec<_>>()
        .join(" ")
}
