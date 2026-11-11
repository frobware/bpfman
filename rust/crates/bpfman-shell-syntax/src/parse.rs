//! The incremental, effect-free parser entry point.

use std::error::Error;
use std::fmt;

use bpfman_shell_source::{Position, SourceName, Span};

use crate::ast::{
    AssertionClause, AssertionMode, BindMode, Command, CommandOutcome, Identifier, ParsedProgram,
    Statement, StatementKind,
};
use crate::compound::parse_statements;
use crate::expr::{parse_command_args, parse_expression};
use crate::token::{LexError, Token, TokenKind, tokenise_at};

/// Broad category of a syntax failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxErrorKind {
    /// Tokenisation rejected the source.
    Lexical,
    /// A fully implemented form has invalid syntax.
    Invalid,
}

/// A located syntax failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyntaxError {
    kind: SyntaxErrorKind,
    span: Span,
    message: String,
}

impl SyntaxError {
    fn new(kind: SyntaxErrorKind, span: Span, message: impl Into<String>) -> Self {
        Self {
            kind,
            span,
            message: message.into(),
        }
    }

    /// Error category, including incomplete parser coverage.
    #[must_use]
    pub fn kind(&self) -> SyntaxErrorKind {
        self.kind
    }

    /// The offending source span.
    #[must_use]
    pub fn span(&self) -> &Span {
        &self.span
    }

    /// Plain diagnostic text, without a rendered location.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}",
            self.span.start().name().as_str(),
            self.span.start().line(),
            self.span.start().column(),
            self.message
        )
    }
}

impl Error for SyntaxError {}

impl From<LexError> for SyntaxError {
    fn from(error: LexError) -> Self {
        Self::new(SyntaxErrorKind::Lexical, error.span, error.message)
    }
}

/// Parse one source unit. The caller supplies only its name and UTF-8 text.
///
/// This parser slice accepts command statements, core expressions, and the
/// `let`, `guard`, `defer`, `return`, `break`, `continue`, `if`, `foreach`,
/// `def`, `poll`, `retry`, `assert`, `require`, record literals, matches blocks,
/// and bind-collect loops. Syntax acceptance does not imply the program has
/// passed semantic checking.
pub fn parse(name: impl Into<SourceName>, text: &str) -> Result<ParsedProgram, SyntaxError> {
    let name = name.into();
    let origin = Position::origin(name.clone());
    let tokens = tokenise_at(origin.clone(), text)?;
    let span = match (tokens.first(), tokens.last()) {
        (Some(first), Some(last)) => Span::new(first.span.start().clone(), last.span.end().clone())
            .map_err(|_| {
                SyntaxError::new(
                    SyntaxErrorKind::Invalid,
                    Span::point(origin.clone()),
                    "source span overflow",
                )
            })?,
        _ => Span::point(origin),
    };
    let statements = parse_statements(&tokens)?;
    Ok(ParsedProgram::new(name, statements, span))
}

pub(crate) fn parse_statement(tokens: &[Token]) -> Result<Statement, SyntaxError> {
    let first = &tokens[0];
    let span = span_of(tokens, &first.span)?;
    let kind = match &first.kind {
        TokenKind::Word(head) => match head.as_str() {
            "let" => parse_let(tokens)?,
            "guard" => parse_guard(tokens)?,
            "defer" => StatementKind::Defer(parse_command(&tokens[1..], &first.span)?),
            "retry" => parse_retry(tokens)?,
            "assert" => parse_assertion(tokens, AssertionMode::Assert)?,
            "require" => parse_assertion(tokens, AssertionMode::Require)?,
            "return" => {
                if tokens.len() == 1 {
                    return Err(invalid(
                        &first.span,
                        "return requires an expression: return EXPR",
                    ));
                }
                StatementKind::Return(parse_expression(&tokens[1..], &first.span)?)
            }
            "break" => {
                no_arguments(tokens, "break")?;
                StatementKind::Break
            }
            "continue" => {
                no_arguments(tokens, "continue")?;
                StatementKind::Continue
            }
            "if" | "elif" | "else" | "foreach" | "poll" | "def" => {
                return Err(invalid(&first.span, format!("incomplete {head} statement")));
            }
            "{" | "}" => {
                return Err(invalid(
                    &first.span,
                    format!("unexpected {head:?} at statement start"),
                ));
            }
            "[" => {
                return Err(invalid(
                    &first.span,
                    "list literal at statement position is not allowed",
                ));
            }
            "(" | "not" | "not-empty" | "true" | "false" => {
                StatementKind::Expression(parse_expression(tokens, &first.span)?)
            }
            _ => StatementKind::Command(parse_command(tokens, &first.span)?),
        },
        TokenKind::Variable(_)
        | TokenKind::Quoted(_)
        | TokenKind::Adapter { .. }
        | TokenKind::Interpolated(_) => {
            StatementKind::Expression(parse_expression(tokens, &first.span)?)
        }
        _ => return Err(invalid(&first.span, "expected a statement")),
    };
    Ok(Statement::new(kind, span))
}

fn parse_let(tokens: &[Token]) -> Result<StatementKind, SyntaxError> {
    let keyword = &tokens[0];
    let Some(name_token) = tokens.get(1) else {
        return Err(invalid(
            &keyword.span,
            "let requires: let <name> = <expr> or let <name> <- <command...>",
        ));
    };
    if matches!(&name_token.kind, TokenKind::Word(word) if word == "(") {
        return parse_let_destructure(tokens);
    }
    let name = identifier(name_token)?;
    let Some(operator) = tokens.get(2) else {
        return Err(invalid(
            &keyword.span,
            "let requires '=' or '<-' after the name",
        ));
    };
    match operator.kind {
        TokenKind::Assign => {
            if name.text() == "_" {
                return Err(invalid(
                    &name_token.span,
                    "single-name let cannot bind '_'; use a real name",
                ));
            }
            if tokens.len() == 3 {
                return Err(invalid(
                    &keyword.span,
                    "let requires: let <name> = <value...>",
                ));
            }
            let value = parse_expression(&tokens[3..], &operator.span)?;
            Ok(StatementKind::Let { name, value })
        }
        TokenKind::Bind => {
            if matches!(tokens.get(3).map(|token| &token.kind), Some(TokenKind::Word(word)) if word == "foreach")
            {
                return Err(invalid(
                    &tokens[3].span,
                    "bind-collect requires a foreach body",
                ));
            }
            validate_bind_rhs(&tokens[3..], &operator.span)?;
            let command = parse_command(&tokens[3..], &operator.span)?;
            Ok(StatementKind::Bind {
                mode: BindMode::Let,
                name,
                command,
            })
        }
        _ => Err(invalid(
            &operator.span,
            "let requires '=' or '<-' after the name",
        )),
    }
}

fn parse_let_destructure(tokens: &[Token]) -> Result<StatementKind, SyntaxError> {
    let open = &tokens[1];
    let mut names: Vec<Identifier> = Vec::new();
    let mut pos = 2usize;
    loop {
        while matches!(
            tokens.get(pos).map(|t| &t.kind),
            Some(TokenKind::Separator(_))
        ) {
            pos += 1;
        }
        let Some(token) = tokens.get(pos) else {
            return Err(invalid(
                &open.span,
                "let: unterminated name list (missing ')')",
            ));
        };
        if matches!(&token.kind, TokenKind::Word(word) if word == ")") {
            pos += 1;
            break;
        }
        let TokenKind::Word(text) = &token.kind else {
            return Err(invalid(&token.span, "let: expected name"));
        };
        if text.contains(',') {
            return Err(invalid(
                &token.span,
                "let: comma is not a separator; use whitespace",
            ));
        }
        let name = Identifier::new(text.clone(), token.span.clone())
            .ok_or_else(|| invalid(&token.span, format!("let: invalid name {text:?}")))?;
        if name.text() != "_" && names.iter().any(|old| old.text() == name.text()) {
            return Err(invalid(
                &token.span,
                format!("let: duplicate name {text:?}"),
            ));
        }
        names.push(name);
        pos += 1;
    }
    if names.len() < 2 {
        return Err(invalid(
            &open.span,
            "let: parenthesised name list requires at least two names",
        ));
    }
    let Some(operator) = tokens.get(pos) else {
        return Err(invalid(
            &open.span,
            "let: expected '=' or '<-' after name list",
        ));
    };
    match operator.kind {
        TokenKind::Bind => Err(invalid(
            &open.span,
            "tuple bind after '<-' is no longer supported",
        )),
        TokenKind::Assign => {
            if names.iter().all(|name| name.text() == "_") {
                return Err(invalid(
                    &open.span,
                    "let: all destructure slots are '_'; at least one must bind",
                ));
            }
            if tokens.len() == pos + 1 {
                return Err(invalid(
                    &tokens[0].span,
                    "let requires: let (<names>) = <value...>",
                ));
            }
            let value = parse_expression(&tokens[pos + 1..], &operator.span)?;
            Ok(StatementKind::LetDestructure { names, value })
        }
        _ => Err(invalid(
            &operator.span,
            "let: expected '=' or '<-' after name list",
        )),
    }
}

fn parse_guard(tokens: &[Token]) -> Result<StatementKind, SyntaxError> {
    let keyword = &tokens[0];
    let Some(name_token) = tokens.get(1) else {
        return Err(invalid(
            &keyword.span,
            "guard requires: guard <name> <- <command...>",
        ));
    };
    if matches!(&name_token.kind, TokenKind::Word(word) if word == "(") {
        return Err(invalid(
            &name_token.span,
            "tuple bind after '<-' is no longer supported",
        ));
    }
    if !matches!(name_token.kind, TokenKind::Word(_)) {
        return Err(invalid(&keyword.span, "guard requires an identifier"));
    }
    let name = identifier(name_token)?;
    let Some(operator) = tokens.get(2) else {
        return Err(invalid(&keyword.span, "guard requires '<-' after the name"));
    };
    if !matches!(operator.kind, TokenKind::Bind) {
        return Err(invalid(
            &operator.span,
            "guard requires '<-' after the name",
        ));
    }
    if matches!(tokens.get(3).map(|token| &token.kind), Some(TokenKind::Word(word)) if word == "foreach")
    {
        return Err(invalid(
            &tokens[3].span,
            "bind-collect requires a foreach body",
        ));
    }
    validate_bind_rhs(&tokens[3..], &operator.span)?;
    let command = parse_command(&tokens[3..], &operator.span)?;
    Ok(StatementKind::Bind {
        mode: BindMode::Guard,
        name,
        command,
    })
}

fn parse_retry(tokens: &[Token]) -> Result<StatementKind, SyntaxError> {
    let keyword = &tokens[0];
    let tail = &tokens[1..];
    let split = tail
        .iter()
        .position(|token| matches!(&token.kind, TokenKind::Word(word) if word == "unless"));
    let (message_tokens, unless_tokens) = if let Some(index) = split {
        (&tail[..index], Some(&tail[index + 1..]))
    } else {
        (tail, None)
    };
    let message = if message_tokens.is_empty() {
        None
    } else {
        Some(parse_expression(message_tokens, &keyword.span)?)
    };
    let unless = match unless_tokens {
        Some([]) => {
            return Err(invalid(
                &keyword.span,
                "retry unless requires a condition expression",
            ));
        }
        Some(tokens) => Some(parse_expression(tokens, &keyword.span)?),
        None => None,
    };
    Ok(StatementKind::Retry { message, unless })
}

fn parse_assertion(tokens: &[Token], mode: AssertionMode) -> Result<StatementKind, SyntaxError> {
    let keyword = &tokens[0];
    let target = &tokens[1..];
    if target.is_empty() {
        return Err(invalid(&keyword.span, "assertion requires a target"));
    }
    if let Some(assign) = target
        .iter()
        .find(|token| matches!(token.kind, TokenKind::Assign))
    {
        return Err(invalid(
            &assign.span,
            "unexpected '='; use let for assignment",
        ));
    }
    let negated = matches!(&target[0].kind, TokenKind::Word(word) if word == "not");
    let body = if negated { &target[1..] } else { target };
    if body.is_empty() {
        return Err(invalid(&target[0].span, "expected a form after 'not'"));
    }
    let clause = match &body[0].kind {
        TokenKind::Word(word) if word == "ok" || word == "fail" => {
            let outcome = if word == "ok" {
                CommandOutcome::Ok
            } else {
                CommandOutcome::Fail
            };
            AssertionClause::CommandStatus {
                outcome,
                head_span: body[0].span.clone(),
                arguments: parse_command_args(&body[1..])?,
                negated,
            }
        }
        _ => AssertionClause::Expression(parse_expression(target, &keyword.span)?),
    };
    Ok(StatementKind::Assertion { mode, clause })
}

fn identifier(token: &Token) -> Result<Identifier, SyntaxError> {
    let TokenKind::Word(text) = &token.kind else {
        return Err(invalid(&token.span, "expected an identifier"));
    };
    Identifier::new(text.clone(), token.span.clone())
        .ok_or_else(|| invalid(&token.span, format!("invalid variable name: {text:?}")))
}

fn parse_command(tokens: &[Token], location: &Span) -> Result<Command, SyntaxError> {
    let Some(head) = tokens.first() else {
        return Err(invalid(location, "command form requires a command head"));
    };
    let mut values = parse_command_args(tokens)?.into_iter();
    let head_value = values
        .next()
        .ok_or_else(|| invalid(&head.span, "command form requires a command head"))?;
    Ok(Command::new(
        head_value,
        values.collect(),
        span_of(tokens, location)?,
    ))
}

fn validate_bind_rhs(tokens: &[Token], operator: &Span) -> Result<(), SyntaxError> {
    if tokens.is_empty() {
        return Err(invalid(operator, "bind requires a command after '<-'"));
    }
    for token in tokens {
        match token.kind {
            TokenKind::Assign => return Err(invalid(&token.span, "unexpected '=' on bind RHS")),
            TokenKind::Bind => return Err(invalid(&token.span, "unexpected '<-' on bind RHS")),
            _ => {}
        }
    }
    Ok(())
}

fn no_arguments(tokens: &[Token], name: &str) -> Result<(), SyntaxError> {
    if let Some(extra) = tokens.get(1) {
        return Err(invalid(&extra.span, format!("{name} takes no arguments")));
    }
    Ok(())
}

fn span_of(tokens: &[Token], fallback: &Span) -> Result<Span, SyntaxError> {
    let Some(first) = tokens.first() else {
        return Err(invalid(fallback, "missing syntax"));
    };
    let last = tokens.last().unwrap_or(first);
    Span::new(first.span.start().clone(), last.span.end().clone())
        .map_err(|_| invalid(&first.span, "source span overflow"))
}

pub(crate) fn invalid(span: &Span, message: impl Into<String>) -> SyntaxError {
    SyntaxError::new(SyntaxErrorKind::Invalid, span.clone(), message)
}

#[cfg(test)]
mod tests {
    use super::{SyntaxErrorKind, parse};
    use crate::ast::{
        AssertionClause, AssertionMode, BindMode, CommandOutcome, ExpressionKind, PureBuiltin,
        StatementKind,
    };

    #[test]
    fn let_destructure_matches_go_name_list_contract() {
        for (source, names) in [
            ("let (a b) = $xs", &["a", "b"][..]),
            ("let (a _ c) = $xs", &["a", "_", "c"][..]),
            ("let (a\n b\n c) = [1 2 3]", &["a", "b", "c"][..]),
        ] {
            let program = parse("destructure.bpfman", source).unwrap_or_else(|_| unreachable!());
            let StatementKind::LetDestructure { names: actual, .. } =
                program.statements()[0].kind()
            else {
                unreachable!()
            };
            assert_eq!(
                actual
                    .iter()
                    .map(crate::ast::Identifier::text)
                    .collect::<Vec<_>>(),
                names
            );
        }
        for (source, message) in [
            ("let (x) = $xs", "at least two names"),
            ("let (_ _) = $xs", "at least one must bind"),
            ("let (a a) = $xs", "duplicate name"),
            ("let (a, b) = $xs", "comma is not a separator"),
            ("let (a b) <- cmd", "tuple bind after '<-'"),
            ("let (a b) =", "requires"),
        ] {
            let error = parse("destructure.bpfman", source)
                .err()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(error.kind(), SyntaxErrorKind::Invalid);
            assert!(error.message().contains(message), "{source}: {error}");
        }
    }

    #[test]
    fn empty_source_has_a_valid_program_and_no_statements() {
        let program = parse("<stdin>", "").unwrap_or_else(|_| unreachable!());
        assert_eq!(program.source().as_str(), "<stdin>");
        assert!(program.statements().is_empty());
        assert_eq!(program.span().start(), program.span().end());
    }

    #[test]
    fn commands_keep_argv_shape_and_byte_spans() {
        let program =
            parse("main.bpfman", "print \"hi\" $out.id\n").unwrap_or_else(|_| unreachable!());
        let [statement] = program.statements() else {
            unreachable!()
        };
        let StatementKind::Command(command) = statement.kind() else {
            unreachable!()
        };
        assert!(matches!(command.head().kind(), ExpressionKind::Word(text) if text == "print"));
        assert!(
            matches!(command.arguments()[0].kind(), ExpressionKind::Quoted(text) if text == "hi")
        );
        assert!(
            matches!(command.arguments()[1].kind(), ExpressionKind::Variable(var) if var.name == "out" && var.path == "id")
        );
        assert_eq!(
            (
                statement.span().start().line(),
                statement.span().start().column()
            ),
            (1, 1)
        );
        assert_eq!(
            (
                statement.span().end().line(),
                statement.span().end().column()
            ),
            (1, 19)
        );
    }

    #[test]
    fn let_guard_and_defer_have_distinct_structural_forms() {
        let program = parse("main.bpfman", "let name = \"demo\"\nlet result <- bpfman version\nguard live <- bpfman program get $id\ndefer print done")
            .unwrap_or_else(|_| unreachable!());
        let [first, second, third, fourth] = program.statements() else {
            unreachable!()
        };
        assert!(
            matches!(first.kind(), StatementKind::Let { name, value } if name.text() == "name" && matches!(value.kind(), ExpressionKind::Quoted(text) if text == "demo"))
        );
        assert!(
            matches!(second.kind(), StatementKind::Bind { mode: BindMode::Let, name, .. } if name.text() == "result")
        );
        assert!(
            matches!(third.kind(), StatementKind::Bind { mode: BindMode::Guard, name, .. } if name.text() == "live")
        );
        assert!(
            matches!(fourth.kind(), StatementKind::Defer(command) if matches!(command.head().kind(), ExpressionKind::Word(text) if text == "print"))
        );
    }

    #[test]
    fn bind_and_defer_preserve_computed_command_heads() {
        let program = parse(
            "main.bpfman",
            "let result <- $command arg\ndefer $cleanup done",
        )
        .unwrap_or_else(|_| unreachable!());
        let [bind, defer] = program.statements() else {
            unreachable!()
        };
        assert!(
            matches!(bind.kind(), StatementKind::Bind { command, .. } if matches!(command.head().kind(), ExpressionKind::Variable(var) if var.name == "command"))
        );
        assert!(
            matches!(defer.kind(), StatementKind::Defer(command) if matches!(command.head().kind(), ExpressionKind::Variable(var) if var.name == "cleanup"))
        );
    }

    #[test]
    fn missing_parts_fail_at_the_relevant_token() {
        for (source, expected) in [
            ("let _ = 1", "single-name let cannot bind"),
            ("let x =", "let requires"),
            ("let x = a = b", "unexpected token after expression"),
            ("let x = load file", "unexpected token after expression"),
            ("guard x = print hi", "guard requires '<-'"),
            ("guard <- print hi", "guard requires an identifier"),
            ("let x <-", "bind requires a command"),
            ("let x <- foo <- bar", "unexpected '<-' on bind RHS"),
            ("break now", "break takes no arguments"),
            ("print = value", "unexpected '='"),
        ] {
            let error = parse("main.bpfman", source)
                .err()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(error.kind(), SyntaxErrorKind::Invalid);
            assert!(error.message().contains(expected), "{source}: {error}");
            assert_eq!(error.span().start().name().as_str(), "main.bpfman");
        }
    }

    #[test]
    fn malformed_compound_expressions_are_never_reinterpreted_as_commands() {
        for source in [
            "let xs <- foreach n in $items {",
            "let result = $x matches { item: true",
        ] {
            let error = parse("main.bpfman", source)
                .err()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(error.kind(), SyntaxErrorKind::Invalid, "{source}: {error}");
        }
    }

    #[test]
    fn lexical_errors_retain_their_category_and_source() {
        let error = parse("main.bpfman", "print $x.")
            .err()
            .unwrap_or_else(|| unreachable!());
        assert_eq!(error.kind(), SyntaxErrorKind::Lexical);
        assert_eq!(error.span().start().name().as_str(), "main.bpfman");
        assert!(error.message().contains("expected identifier after '.'"));
    }

    #[test]
    fn assertions_distinguish_boolean_and_command_status_clauses() {
        let program = parse("main.bpfman", "assert contains $rc.stderr 'boom'\nassert not $ok\nassert not ok exec false\nrequire fail exec true")
            .unwrap_or_else(|_| unreachable!());
        let [first, second, third, fourth] = program.statements() else {
            unreachable!()
        };
        assert!(
            matches!(first.kind(), StatementKind::Assertion { mode: AssertionMode::Assert, clause: AssertionClause::Expression(value) } if matches!(value.kind(), ExpressionKind::PureCall { builtin: PureBuiltin::Contains, .. }))
        );
        assert!(
            matches!(second.kind(), StatementKind::Assertion { clause: AssertionClause::Expression(value), .. } if matches!(value.kind(), ExpressionKind::Not(_)))
        );
        assert!(
            matches!(third.kind(), StatementKind::Assertion { clause: AssertionClause::CommandStatus { outcome: CommandOutcome::Ok, negated: true, arguments, head_span }, .. } if arguments.len() == 2 && head_span.start().column() == 12)
        );
        assert!(matches!(
            fourth.kind(),
            StatementKind::Assertion {
                mode: AssertionMode::Require,
                clause: AssertionClause::CommandStatus {
                    outcome: CommandOutcome::Fail,
                    negated: false,
                    ..
                }
            }
        ));
    }

    #[test]
    fn assertion_syntax_keeps_checker_responsibilities_separate() {
        let program = parse("main.bpfman", "assert ok").unwrap_or_else(|_| unreachable!());
        assert!(
            matches!(program.statements()[0].kind(), StatementKind::Assertion { clause: AssertionClause::CommandStatus { arguments, .. }, .. } if arguments.is_empty())
        );
        for (source, message) in [
            ("assert", "requires a target"),
            ("require not", "expected a form after 'not'"),
            ("assert nil", "nil has been removed"),
            ("assert $a = $b", "unexpected '='"),
        ] {
            let error = parse("main.bpfman", source)
                .err()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(error.kind(), SyntaxErrorKind::Invalid, "{source}");
            assert!(error.message().contains(message), "{source}: {error}");
        }
    }
}
