//! The incremental, effect-free parser entry point.

use std::error::Error;
use std::fmt;

use bpfman_shell_source::{Position, SourceName, Span};

use crate::ast::{
    BindMode, Command, CommandHead, Expression, ExpressionKind, Identifier, ParsedProgram,
    Statement, StatementKind,
};
use crate::token::{LexError, Token, TokenKind, tokenise_at};

/// Broad category of a syntax failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyntaxErrorKind {
    /// Tokenisation rejected the source.
    Lexical,
    /// A fully implemented form has invalid syntax.
    Invalid,
    /// This parser slice has not classified the form yet.
    Unimplemented,
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
/// This parser slice accepts command statements and the simple `let`, `guard`,
/// `defer`, `return`, `break`, and `continue` forms. Other forms may return
/// [`SyntaxErrorKind::Unimplemented`], including forms whose validity has not
/// yet been checked.
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
    let mut statements = Vec::new();
    let mut cursor = 0;
    while cursor < tokens.len() {
        if matches!(tokens[cursor].kind, TokenKind::Separator(_)) {
            cursor += 1;
            continue;
        }
        let start = cursor;
        while cursor < tokens.len() && !matches!(tokens[cursor].kind, TokenKind::Separator(_)) {
            cursor += 1;
        }
        statements.push(parse_statement(&tokens[start..cursor])?);
    }
    Ok(ParsedProgram::new(name, statements, span))
}

fn parse_statement(tokens: &[Token]) -> Result<Statement, SyntaxError> {
    let first = &tokens[0];
    let span = span_of(tokens, &first.span)?;
    let kind = match &first.kind {
        TokenKind::Word(head) => match head.as_str() {
            "let" => parse_let(tokens)?,
            "guard" => parse_guard(tokens)?,
            "defer" => StatementKind::Defer(parse_command(&tokens[1..], &first.span)?),
            "return" => {
                if tokens.len() == 1 {
                    return Err(invalid(
                        &first.span,
                        "return requires an expression: return EXPR",
                    ));
                }
                StatementKind::Return(parse_one_expression(&tokens[1..], &first.span)?)
            }
            "break" => {
                no_arguments(tokens, "break")?;
                StatementKind::Break
            }
            "continue" => {
                no_arguments(tokens, "continue")?;
                StatementKind::Continue
            }
            "if" | "elif" | "else" | "foreach" | "poll" | "retry" | "def" | "assert"
            | "require" => {
                return Err(unimplemented(&first.span, format!("{head} statement")));
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
                return Err(unimplemented(&first.span, "expression statement"));
            }
            _ => StatementKind::Command(parse_command(tokens, &first.span)?),
        },
        TokenKind::Variable(_)
        | TokenKind::Quoted(_)
        | TokenKind::Adapter { .. }
        | TokenKind::Interpolated(_) => {
            return Err(unimplemented(&first.span, "expression statement"));
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
        return Err(unimplemented(&name_token.span, "let destructuring"));
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
            let value = parse_one_expression(&tokens[3..], &operator.span)?;
            Ok(StatementKind::Let { name, value })
        }
        TokenKind::Bind => {
            if matches!(tokens.get(3).map(|token| &token.kind), Some(TokenKind::Word(word)) if word == "foreach")
            {
                return Err(unimplemented(&tokens[3].span, "bind-collect foreach"));
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
        return Err(unimplemented(&tokens[3].span, "bind-collect foreach"));
    }
    validate_bind_rhs(&tokens[3..], &operator.span)?;
    let command = parse_command(&tokens[3..], &operator.span)?;
    Ok(StatementKind::Bind {
        mode: BindMode::Guard,
        name,
        command,
    })
}

fn identifier(token: &Token) -> Result<Identifier, SyntaxError> {
    let TokenKind::Word(text) = &token.kind else {
        return Err(invalid(&token.span, "expected an identifier"));
    };
    Identifier::new(text.clone(), token.span.clone())
        .ok_or_else(|| invalid(&token.span, format!("invalid variable name: {text:?}")))
}

fn parse_command(tokens: &[Token], location: &Span) -> Result<Command, SyntaxError> {
    let Some((head, tail)) = tokens.split_first() else {
        return Err(invalid(location, "command form requires a command head"));
    };
    let TokenKind::Word(word) = &head.kind else {
        return Err(invalid(&head.span, "command head must be a word"));
    };
    let head = CommandHead::new(word.clone(), head.span.clone())
        .ok_or_else(|| invalid(&head.span, "command head must be a word"))?;
    let mut arguments = Vec::with_capacity(tail.len());
    for token in tail {
        arguments.push(parse_atom(token)?);
    }
    Ok(Command::new(head, arguments, span_of(tokens, location)?))
}

fn parse_one_expression(tokens: &[Token], location: &Span) -> Result<Expression, SyntaxError> {
    let Some((first, rest)) = tokens.split_first() else {
        return Err(invalid(location, "expected an expression"));
    };
    if let Some(stray_assign) = rest
        .iter()
        .find(|token| matches!(token.kind, TokenKind::Assign))
    {
        return Err(invalid(&stray_assign.span, "unexpected '=' in let RHS"));
    }
    if !rest.is_empty() {
        if matches!(&rest[0].kind, TokenKind::Word(word) if !matches!(word.as_str(), "+" | "-" | "*" | "/" | "%" | "==" | "!=" | "<" | "<=" | ">" | ">=" | "and" | "or" | "matches"))
            && !matches!(&first.kind, TokenKind::Word(word) if matches!(word.as_str(), "jq" | "range" | "zip" | "record" | "not" | "not-empty"))
        {
            return Err(invalid(&rest[0].span, "unexpected token after expression"));
        }
        return Err(unimplemented(&rest[0].span, "compound expression"));
    }
    parse_atom(first)
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

fn parse_atom(token: &Token) -> Result<Expression, SyntaxError> {
    let kind = match &token.kind {
        TokenKind::Word(text) if !matches!(text.as_str(), "(" | ")" | "[" | "]" | "{" | "}") => {
            ExpressionKind::Word(text.clone())
        }
        TokenKind::Quoted(text) => ExpressionKind::Quoted(text.clone()),
        TokenKind::Variable(variable) => ExpressionKind::Variable(variable.clone()),
        TokenKind::Adapter { name, variable } => ExpressionKind::Adapter {
            name: name.clone(),
            variable: variable.clone(),
        },
        TokenKind::Interpolated(_) => {
            return Err(unimplemented(&token.span, "interpolated expression"));
        }
        TokenKind::Word(_) => return Err(unimplemented(&token.span, "grouped or list expression")),
        TokenKind::Assign => {
            return Err(invalid(
                &token.span,
                "unexpected '='; use let for assignment",
            ));
        }
        TokenKind::Bind => return Err(invalid(&token.span, "unexpected '<-'")),
        TokenKind::Thread => return Err(unimplemented(&token.span, "thread expression")),
        TokenKind::Separator(_) => return Err(invalid(&token.span, "unexpected separator")),
    };
    Ok(Expression::new(kind, token.span.clone()))
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

fn invalid(span: &Span, message: impl Into<String>) -> SyntaxError {
    SyntaxError::new(SyntaxErrorKind::Invalid, span.clone(), message)
}

fn unimplemented(span: &Span, feature: impl Into<String>) -> SyntaxError {
    SyntaxError::new(
        SyntaxErrorKind::Unimplemented,
        span.clone(),
        format!(
            "{} is not implemented in the Rust parser yet",
            feature.into()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::{SyntaxErrorKind, parse};
    use crate::ast::{BindMode, ExpressionKind, StatementKind};

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
        assert_eq!(command.head().text(), "print");
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
            matches!(fourth.kind(), StatementKind::Defer(command) if command.head().text() == "print")
        );
    }

    #[test]
    fn missing_parts_fail_at_the_relevant_token() {
        for (source, expected) in [
            ("let _ = 1", "single-name let cannot bind"),
            ("let x =", "let requires"),
            ("let x = a = b", "unexpected '=' in let RHS"),
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
    fn incomplete_language_constructs_are_never_reinterpreted_as_commands() {
        for source in [
            "if $ready {",
            "let x = 1 + 2",
            "let xs <- foreach n in $items {",
            "print ($x |> jq .id)",
        ] {
            let error = parse("main.bpfman", source)
                .err()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(
                error.kind(),
                SyntaxErrorKind::Unimplemented,
                "{source}: {error}"
            );
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
}
