//! Expression lowering. This module owns precedence and consumes no I/O.

use bpfman_shell_source::{Position, SourceText, Span};

use crate::ast::{
    BinaryOperator, Expression, ExpressionKind, InterpolatedPart, LogicalOperator, PureBuiltin,
    ThreadCall,
};
use crate::parse::{SyntaxError, invalid, unimplemented};
use crate::token::{Segment, Token, TokenKind, tokenise, tokenise_at};

pub(crate) fn parse_expression(
    tokens: &[Token],
    location: &Span,
) -> Result<Expression, SyntaxError> {
    let tokens = tokens
        .iter()
        .filter(|token| !matches!(token.kind, TokenKind::Separator(_)))
        .cloned()
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return Err(invalid(location, "expected an expression"));
    }
    let mut parser = Cursor {
        tokens: &tokens,
        pos: 0,
    };
    let expression = parser.or()?;
    if let Some(token) = parser.peek() {
        return Err(invalid(&token.span, "unexpected token after expression"));
    }
    Ok(expression)
}

pub(crate) fn parse_command_args(tokens: &[Token]) -> Result<Vec<Expression>, SyntaxError> {
    let mut parser = Cursor { tokens, pos: 0 };
    let mut args = Vec::new();
    while let Some(token) = parser.peek() {
        if word(token, "(") {
            let open = parser
                .take()
                .ok_or_else(|| invalid(&tokens[0].span, "unexpected end of input"))?;
            let value = parser.group(&open.span)?;
            args.push(value);
        } else if word(token, "[") {
            args.push(parser.list()?);
        } else {
            args.push(primary(token)?);
            parser.pos += 1;
        }
    }
    Ok(args)
}

pub(crate) fn primary(token: &Token) -> Result<Expression, SyntaxError> {
    let kind = match &token.kind {
        TokenKind::Word(text) if !matches!(text.as_str(), "(" | ")" | "[" | "]" | "{" | "}") => {
            if text == "nil" {
                return Err(invalid(&token.span, "nil has been removed; use null"));
            }
            ExpressionKind::Word(text.clone())
        }
        TokenKind::Quoted(text) => ExpressionKind::Quoted(text.clone()),
        TokenKind::Variable(variable) => ExpressionKind::Variable(variable.clone()),
        TokenKind::Adapter { name, variable } => ExpressionKind::Adapter {
            name: name.clone(),
            variable: variable.clone(),
        },
        TokenKind::Interpolated(segments) => {
            let mut parts = Vec::with_capacity(segments.len());
            for segment in segments {
                match segment {
                    Segment::Literal { text, .. } => {
                        parts.push(InterpolatedPart::Literal(text.clone()))
                    }
                    Segment::Expression(interpolation) => {
                        let value = parse_interpolation(&interpolation.inner, &interpolation.span)?;
                        parts.push(InterpolatedPart::Expression(Box::new(value)));
                    }
                }
            }
            ExpressionKind::Interpolated(parts)
        }
        TokenKind::Assign => {
            return Err(invalid(
                &token.span,
                "unexpected '='; use let for assignment",
            ));
        }
        TokenKind::Bind => return Err(invalid(&token.span, "unexpected '<-'")),
        TokenKind::Thread => return Err(invalid(&token.span, "unexpected '|>'")),
        TokenKind::Separator(_) => return Err(invalid(&token.span, "unexpected separator")),
        TokenKind::Word(_) => return Err(invalid(&token.span, "unexpected bracket in expression")),
    };
    Ok(Expression::new(kind, token.span.clone()))
}

fn parse_interpolation(inner: &str, outer: &Span) -> Result<Expression, SyntaxError> {
    let trimmed = inner.trim();
    let leading = inner.len() - inner.trim_start().len();
    let body_column = outer
        .start()
        .column()
        .checked_add(2)
        .ok_or_else(|| invalid(outer, "source position overflow"))?;
    let body_start = Position::new(
        outer.start().name().clone(),
        outer.start().line(),
        body_column,
    )
    .ok_or_else(|| invalid(outer, "source position overflow"))?;
    let source = SourceText::new(body_start, inner);
    let start = source
        .position_at(leading)
        .ok_or_else(|| invalid(outer, "source position overflow"))?;
    let end = source
        .position_at(leading + trimmed.len())
        .ok_or_else(|| invalid(outer, "source position overflow"))?;
    let body_span =
        Span::new(start.clone(), end).map_err(|_| invalid(outer, "source span overflow"))?;
    if !trimmed.starts_with('$') {
        let synthetic = format!("${trimmed}");
        if let Ok(tokens) = tokenise(&synthetic) {
            if let [
                Token {
                    kind: TokenKind::Variable(variable),
                    ..
                },
            ] = tokens.as_slice()
            {
                return Ok(Expression::new(
                    ExpressionKind::Variable(variable.clone()),
                    body_span,
                ));
            }
        }
    }
    let tokens = tokenise_at(start, trimmed)?;
    parse_expression(&tokens, &body_span)
}

fn word(token: &Token, expected: &str) -> bool {
    matches!(&token.kind, TokenKind::Word(text) if text == expected)
}

fn binary(token: &Token) -> Option<BinaryOperator> {
    let TokenKind::Word(text) = &token.kind else {
        return None;
    };
    Some(match text.as_str() {
        "==" => BinaryOperator::Equal,
        "!=" => BinaryOperator::NotEqual,
        "<" => BinaryOperator::Less,
        "<=" => BinaryOperator::LessEqual,
        ">" => BinaryOperator::Greater,
        ">=" => BinaryOperator::GreaterEqual,
        "+" => BinaryOperator::Add,
        "-" => BinaryOperator::Subtract,
        "*" => BinaryOperator::Multiply,
        "/" => BinaryOperator::Divide,
        "%" => BinaryOperator::Remainder,
        _ => return None,
    })
}

fn builtin(token: &Token) -> Option<(PureBuiltin, usize)> {
    let TokenKind::Word(text) = &token.kind else {
        return None;
    };
    Some(match text.as_str() {
        "jq" => (PureBuiltin::Jq, 2),
        "u32le" => (PureBuiltin::U32Le, 1),
        "u64le" => (PureBuiltin::U64Le, 1),
        "range" => (PureBuiltin::Range, 1),
        "zip" => (PureBuiltin::Zip, 2),
        "path-exists" => (PureBuiltin::PathExists, 1),
        "contains" => (PureBuiltin::Contains, 2),
        "null" => (PureBuiltin::Null, 1),
        "present" => (PureBuiltin::Present, 1),
        "missing" => (PureBuiltin::Missing, 1),
        "empty" => (PureBuiltin::Empty, 1),
        _ => return None,
    })
}

fn validate_expression_word(token: &Token) -> Result<(), SyntaxError> {
    let TokenKind::Word(text) = &token.kind else {
        return Ok(());
    };
    if text.contains(',') {
        return Err(invalid(&token.span, "unquoted comma in expression literal"));
    }
    let bytes = text.as_bytes();
    let starts_numeric = bytes.first().is_some_and(u8::is_ascii_digit)
        || matches!(bytes.first(), Some(b'+' | b'-'))
            && bytes.get(1).is_some_and(u8::is_ascii_digit);
    if starts_numeric && !json_number(text) {
        return Err(invalid(
            &token.span,
            format!("invalid numeric literal {text:?}"),
        ));
    }
    Ok(())
}

fn json_number(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = usize::from(bytes.first() == Some(&b'-'));
    match bytes.get(i) {
        Some(b'0') => i += 1,
        Some(b'1'..=b'9') => {
            i += 1;
            while bytes.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
        }
        _ => return false,
    }
    let mut fractional = false;
    if bytes.get(i) == Some(&b'.') {
        fractional = true;
        i += 1;
        let begin = i;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == begin {
            return false;
        }
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        fractional = true;
        i += 1;
        if matches!(bytes.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let begin = i;
        while bytes.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == begin {
            return false;
        }
    }
    i == bytes.len() && (!fractional || text.parse::<f64>().is_ok_and(f64::is_finite))
}

fn span(start: &Span, end: &Span) -> Result<Span, SyntaxError> {
    Span::new(start.start().clone(), end.end().clone())
        .map_err(|_| invalid(start, "source span overflow"))
}

struct Cursor<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn take(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.pos)?.clone();
        self.pos += 1;
        Some(token)
    }

    fn at(&self, text: &str) -> bool {
        self.peek().is_some_and(|token| word(token, text))
    }

    fn or(&mut self) -> Result<Expression, SyntaxError> {
        let mut lhs = self.and()?;
        while self.at("or") {
            self.take();
            let rhs = self.and()?;
            let extent = span(lhs.span(), rhs.span())?;
            lhs = Expression::new(
                ExpressionKind::Logical {
                    left: Box::new(lhs),
                    operator: LogicalOperator::Or,
                    right: Box::new(rhs),
                },
                extent,
            );
        }
        Ok(lhs)
    }

    fn and(&mut self) -> Result<Expression, SyntaxError> {
        let mut lhs = self.not()?;
        while self.at("and") {
            self.take();
            let rhs = self.not()?;
            let extent = span(lhs.span(), rhs.span())?;
            lhs = Expression::new(
                ExpressionKind::Logical {
                    left: Box::new(lhs),
                    operator: LogicalOperator::And,
                    right: Box::new(rhs),
                },
                extent,
            );
        }
        Ok(lhs)
    }

    fn not(&mut self) -> Result<Expression, SyntaxError> {
        if self.at("not") {
            let start = self
                .take()
                .ok_or_else(|| invalid(&self.tokens[0].span, "unexpected end of input"))?;
            let operand = self.not()?;
            let extent = span(&start.span, operand.span())?;
            return Ok(Expression::new(
                ExpressionKind::Not(Box::new(operand)),
                extent,
            ));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Result<Expression, SyntaxError> {
        let lhs = self.additive()?;
        if self.at("matches") {
            let token = self
                .take()
                .ok_or_else(|| invalid(&self.tokens[0].span, "unexpected end of input"))?;
            return Err(unimplemented(&token.span, "matches expression"));
        }
        let Some(
            operator @ (BinaryOperator::Equal
            | BinaryOperator::NotEqual
            | BinaryOperator::Less
            | BinaryOperator::LessEqual
            | BinaryOperator::Greater
            | BinaryOperator::GreaterEqual),
        ) = self.peek().and_then(binary)
        else {
            return Ok(lhs);
        };
        self.take();
        let rhs = self.additive()?;
        let extent = span(lhs.span(), rhs.span())?;
        Ok(Expression::new(
            ExpressionKind::Binary {
                left: Box::new(lhs),
                operator,
                right: Box::new(rhs),
            },
            extent,
        ))
    }

    fn additive(&mut self) -> Result<Expression, SyntaxError> {
        let mut lhs = self.multiplicative()?;
        while let Some(operator @ (BinaryOperator::Add | BinaryOperator::Subtract)) =
            self.peek().and_then(binary)
        {
            self.take();
            let rhs = self.multiplicative()?;
            let extent = span(lhs.span(), rhs.span())?;
            lhs = Expression::new(
                ExpressionKind::Binary {
                    left: Box::new(lhs),
                    operator,
                    right: Box::new(rhs),
                },
                extent,
            );
        }
        Ok(lhs)
    }

    fn multiplicative(&mut self) -> Result<Expression, SyntaxError> {
        let mut lhs = self.predicate()?;
        while let Some(
            operator @ (BinaryOperator::Multiply
            | BinaryOperator::Divide
            | BinaryOperator::Remainder),
        ) = self.peek().and_then(binary)
        {
            self.take();
            let rhs = self.predicate()?;
            let extent = span(lhs.span(), rhs.span())?;
            lhs = Expression::new(
                ExpressionKind::Binary {
                    left: Box::new(lhs),
                    operator,
                    right: Box::new(rhs),
                },
                extent,
            );
        }
        Ok(lhs)
    }

    fn predicate(&mut self) -> Result<Expression, SyntaxError> {
        if self.at("not-empty") && self.tokens.get(self.pos + 1).is_some_and(|next| {
            !matches!(next.kind, TokenKind::Thread)
                && !matches!(&next.kind, TokenKind::Word(text) if matches!(text.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=" | "+" | "-" | "*" | "/" | "%" | "and" | "or" | ")"))
        }) {
            let start = self.take().ok_or_else(|| invalid(&self.tokens[0].span, "unexpected end of input"))?;
            let operand = self.term()?;
            let extent = span(&start.span, operand.span())?;
            return Ok(Expression::new(ExpressionKind::NotEmpty(Box::new(operand)), extent));
        }
        self.negate()
    }

    fn negate(&mut self) -> Result<Expression, SyntaxError> {
        if self.at("-") {
            let start = self
                .take()
                .ok_or_else(|| invalid(&self.tokens[0].span, "unexpected end of input"))?;
            let operand = self.negate()?;
            let extent = span(&start.span, operand.span())?;
            return Ok(Expression::new(
                ExpressionKind::Negate(Box::new(operand)),
                extent,
            ));
        }
        self.thread()
    }

    fn thread(&mut self) -> Result<Expression, SyntaxError> {
        let mut value = self.term()?;
        while matches!(self.peek().map(|t| &t.kind), Some(TokenKind::Thread)) {
            let pipe = self
                .take()
                .ok_or_else(|| invalid(&self.tokens[0].span, "unexpected end of input"))?;
            let start = self.pos;
            while let Some(next) = self.peek() {
                if matches!(next.kind, TokenKind::Thread)
                    || word(next, ")")
                    || word(next, "]")
                    || word(next, "}")
                    || word(next, "and")
                    || word(next, "or")
                    || word(next, "matches")
                    || binary(next).is_some()
                {
                    break;
                }
                self.pos += 1;
            }
            let args = &self.tokens[start..self.pos];
            let Some((head, tail)) = args.split_first() else {
                return Err(invalid(
                    &pipe.span,
                    "thread requires a command on the right-hand side",
                ));
            };
            let head_node = primary(head)?;
            let arguments = args[1..]
                .iter()
                .map(primary)
                .collect::<Result<Vec<_>, _>>()?;
            let command = ThreadCall::new(
                head_node,
                arguments,
                span(&head.span, tail.last().map_or(&head.span, |t| &t.span))?,
            );
            let extent = span(value.span(), command.span())?;
            value = Expression::new(
                ExpressionKind::Thread {
                    value: Box::new(value),
                    command,
                    pipe: pipe.span.start().clone(),
                },
                extent,
            );
        }
        Ok(value)
    }

    fn term(&mut self) -> Result<Expression, SyntaxError> {
        let token = self.peek().ok_or_else(|| {
            invalid(
                &self.tokens[self.tokens.len() - 1].span,
                "expected expression, got end of input",
            )
        })?;
        if word(token, "(") {
            let open = self
                .take()
                .ok_or_else(|| invalid(&self.tokens[0].span, "unexpected end of input"))?;
            return self.group(&open.span);
        }
        if word(token, "[") {
            return self.list();
        }
        if word(token, "record") {
            return Err(unimplemented(&token.span, "record literal"));
        }
        if let Some((builtin, arity)) = builtin(token) {
            if builtin != PureBuiltin::Null
                || self.tokens.get(self.pos + 1).is_some_and(can_start_arg)
            {
                let start = self
                    .take()
                    .ok_or_else(|| invalid(&self.tokens[0].span, "unexpected end of input"))?;
                let mut args = Vec::with_capacity(arity);
                for _ in 0..arity {
                    args.push(self.call_arg(&start.span)?);
                }
                let extent = span(
                    &start.span,
                    args.last().map_or(&start.span, Expression::span),
                )?;
                return Ok(Expression::new(
                    ExpressionKind::PureCall {
                        builtin,
                        arguments: args,
                    },
                    extent,
                ));
            }
        }
        validate_expression_word(token)?;
        let token = self
            .take()
            .ok_or_else(|| invalid(&self.tokens[0].span, "unexpected end of input"))?;
        primary(&token)
    }

    fn group(&mut self, open: &Span) -> Result<Expression, SyntaxError> {
        if self.at(")") {
            return Err(invalid(open, "empty parenthesised expression"));
        }
        let value = self.or()?;
        if !self.at(")") {
            return Err(invalid(
                open,
                "missing ')' to close parenthesised expression",
            ));
        }
        self.take();
        Ok(value)
    }

    fn list(&mut self) -> Result<Expression, SyntaxError> {
        let open = self
            .take()
            .ok_or_else(|| invalid(&self.tokens[0].span, "unexpected end of input"))?;
        let mut elements = Vec::new();
        loop {
            let Some(token) = self.peek() else {
                return Err(invalid(&open.span, "missing ']' to close list literal"));
            };
            if word(token, "]") {
                let close = self
                    .take()
                    .ok_or_else(|| invalid(&self.tokens[0].span, "unexpected end of input"))?;
                return Ok(Expression::new(
                    ExpressionKind::List(elements),
                    span(&open.span, &close.span)?,
                ));
            }
            if binary(token).is_some()
                || word(token, "and")
                || word(token, "or")
                || matches!(token.kind, TokenKind::Thread)
            {
                return Err(invalid(
                    &token.span,
                    "operator between list elements; wrap a compound element in parentheses",
                ));
            }
            if matches!(&token.kind, TokenKind::Word(text) if text.contains(',')) {
                return Err(invalid(
                    &token.span,
                    "list elements are whitespace-separated; commas are not separators",
                ));
            }
            elements.push(self.term()?);
        }
    }

    fn call_arg(&mut self, call: &Span) -> Result<Expression, SyntaxError> {
        let Some(token) = self.peek() else {
            return Err(invalid(call, "pure builtin requires another argument"));
        };
        if word(token, "(") || word(token, "[") {
            return self.term();
        }
        if !can_start_arg(token) {
            return Err(invalid(
                &token.span,
                "unexpected token in pure builtin argument",
            ));
        }
        validate_expression_word(token)?;
        let token = self
            .take()
            .ok_or_else(|| invalid(&self.tokens[0].span, "unexpected end of input"))?;
        primary(&token)
    }
}

fn can_start_arg(token: &Token) -> bool {
    match &token.kind {
        TokenKind::Quoted(_)
        | TokenKind::Variable(_)
        | TokenKind::Adapter { .. }
        | TokenKind::Interpolated(_) => true,
        TokenKind::Word(text) => !matches!(
            text.as_str(),
            ")" | "]"
                | "}"
                | "and"
                | "or"
                | "not"
                | "+"
                | "-"
                | "*"
                | "/"
                | "%"
                | "=="
                | "!="
                | "<"
                | "<="
                | ">"
                | ">="
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_expression;
    use crate::ast::{
        BinaryOperator as Op, ExpressionKind as Kind, InterpolatedPart, LogicalOperator,
        PureBuiltin, StatementKind,
    };
    use crate::parse::{SyntaxErrorKind, parse};
    use crate::token::tokenise;

    fn rhs(source: &str) -> crate::ast::Expression {
        let program = parse("expr.bpfman", &format!("let value = {source}"))
            .unwrap_or_else(|_| unreachable!());
        let StatementKind::Let { value, .. } = program.statements()[0].kind() else {
            unreachable!()
        };
        value.clone()
    }

    #[test]
    fn arithmetic_and_comparison_follow_go_precedence() {
        let value = rhs("1 + 2 * 3 == 7");
        let Kind::Binary {
            left,
            operator: Op::Equal,
            right,
        } = value.kind()
        else {
            unreachable!()
        };
        assert!(matches!(right.kind(), Kind::Word(text) if text == "7"));
        let Kind::Binary {
            left,
            operator: Op::Add,
            right,
        } = left.kind()
        else {
            unreachable!()
        };
        assert!(matches!(left.kind(), Kind::Word(text) if text == "1"));
        assert!(matches!(
            right.kind(),
            Kind::Binary {
                operator: Op::Multiply,
                ..
            }
        ));

        let value = rhs("(1 + 2) * 3");
        let Kind::Binary {
            left,
            operator: Op::Multiply,
            ..
        } = value.kind()
        else {
            unreachable!()
        };
        assert!(matches!(
            left.kind(),
            Kind::Binary {
                operator: Op::Add,
                ..
            }
        ));

        let value = rhs("10 - 3 - 2");
        let Kind::Binary {
            left,
            operator: Op::Subtract,
            ..
        } = value.kind()
        else {
            unreachable!()
        };
        assert!(matches!(
            left.kind(),
            Kind::Binary {
                operator: Op::Subtract,
                ..
            }
        ));
    }

    #[test]
    fn boolean_and_unary_precedence_are_explicit() {
        let value = rhs("not $a == $b or $c and not-empty $items");
        let Kind::Logical {
            left,
            operator: LogicalOperator::Or,
            right,
        } = value.kind()
        else {
            unreachable!()
        };
        assert!(
            matches!(left.kind(), Kind::Not(inner) if matches!(inner.kind(), Kind::Binary { operator: Op::Equal, .. }))
        );
        assert!(
            matches!(right.kind(), Kind::Logical { operator: LogicalOperator::And, right, .. } if matches!(right.kind(), Kind::NotEmpty(_)))
        );
        assert!(
            matches!(rhs("- - $n").kind(), Kind::Negate(inner) if matches!(inner.kind(), Kind::Negate(_)))
        );
        assert!(matches!(rhs("not-empty").kind(), Kind::Word(text) if text == "not-empty"));
    }

    #[test]
    fn pure_calls_use_fixed_arity_and_primary_arguments() {
        let value = rhs("range 5 + 1");
        let Kind::Binary {
            left,
            operator: Op::Add,
            ..
        } = value.kind()
        else {
            unreachable!()
        };
        assert!(
            matches!(left.kind(), Kind::PureCall { builtin: PureBuiltin::Range, arguments } if arguments.len() == 1)
        );
        let value = rhs("jq '.id' (zip $left $right)");
        assert!(
            matches!(value.kind(), Kind::PureCall { builtin: PureBuiltin::Jq, arguments } if arguments.len() == 2 && matches!(arguments[1].kind(), Kind::PureCall { builtin: PureBuiltin::Zip, .. }))
        );
        assert!(matches!(rhs("null").kind(), Kind::Word(text) if text == "null"));
        let error = parse("expr.bpfman", "let value = jq '.id'")
            .err()
            .unwrap_or_else(|| unreachable!());
        assert_eq!(error.kind(), SyntaxErrorKind::Invalid);
    }

    #[test]
    fn thread_chains_and_command_arguments_keep_structure() {
        let value = rhs("$snap |> jq '.id' |> u32le");
        let Kind::Thread {
            value,
            command,
            pipe,
        } = value.kind()
        else {
            unreachable!()
        };
        assert_eq!(pipe.column(), 31);
        assert!(matches!(command.head().kind(), Kind::Word(text) if text == "u32le"));
        assert!(
            matches!(value.kind(), Kind::Thread { command, pipe, .. } if pipe.column() == 19 && matches!(command.head().kind(), Kind::Word(text) if text == "jq"))
        );
        let value = rhs("$snap |> $command --verbose");
        assert!(
            matches!(value.kind(), Kind::Thread { command, .. } if matches!(command.head().kind(), Kind::Variable(var) if var.name == "command"))
        );
        let program = parse("expr.bpfman", "print ($snap |> jq '.id') [1 2]")
            .unwrap_or_else(|_| unreachable!());
        let StatementKind::Command(command) = program.statements()[0].kind() else {
            unreachable!()
        };
        assert!(matches!(command.arguments()[0].kind(), Kind::Thread { .. }));
        assert!(matches!(command.arguments()[1].kind(), Kind::List(items) if items.len() == 2));
    }

    #[test]
    fn multiline_nested_lists_and_interpolation_have_source_spans() {
        let value = rhs("[1\n [2 3]]");
        assert!(
            matches!(value.kind(), Kind::List(items) if matches!(items[1].kind(), Kind::List(inner) if inner.len() == 2))
        );
        let program = parse("expr.bpfman", "print \"id=${item.id}, twice=${$n * 2}\"")
            .unwrap_or_else(|_| unreachable!());
        let StatementKind::Command(command) = program.statements()[0].kind() else {
            unreachable!()
        };
        let Kind::Interpolated(parts) = command.arguments()[0].kind() else {
            unreachable!()
        };
        assert!(
            matches!(&parts[1], InterpolatedPart::Expression(value) if matches!(value.kind(), Kind::Variable(var) if var.name == "item" && var.path == "id") && value.span().start().column() == 13)
        );
        assert!(
            matches!(&parts[3], InterpolatedPart::Expression(value) if matches!(value.kind(), Kind::Binary { operator: Op::Multiply, .. }))
        );
    }

    #[test]
    fn incomplete_and_invalid_forms_remain_distinct() {
        for source in [
            "let value = record { x: 1 }",
            "let value = $x matches { x: 1 }",
        ] {
            assert_eq!(
                parse("expr.bpfman", source)
                    .err()
                    .unwrap_or_else(|| unreachable!())
                    .kind(),
                SyntaxErrorKind::Unimplemented
            );
        }
        for source in [
            "let value = [1, 2]",
            "let value = (1 + 2",
            "let value = 1 == 2 == 3",
        ] {
            assert_eq!(
                parse("expr.bpfman", source)
                    .err()
                    .unwrap_or_else(|| unreachable!())
                    .kind(),
                SyntaxErrorKind::Invalid
            );
        }
        let tokens = tokenise("1 + 2").unwrap_or_else(|_| unreachable!());
        assert!(parse_expression(&tokens, &tokens[0].span).is_ok());
    }

    #[test]
    fn expression_numbers_follow_json_grammar_without_restricting_integer_size() {
        for number in [
            "0",
            "-5",
            "3.14",
            "1e3",
            "4026531840",
            "123456789012345678901234567890",
        ] {
            assert!(
                parse("expr.bpfman", &format!("let value = {number}")).is_ok(),
                "{number}"
            );
        }
        for number in ["+5", "05", "0x10", "1e309", "1.10.3", "1,"] {
            let error = parse("expr.bpfman", &format!("let value = {number}"))
                .err()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(error.kind(), SyntaxErrorKind::Invalid, "{number}");
        }
        // Command arguments are CLI words; they do not use expression-literal validation.
        assert!(parse("expr.bpfman", "print 1, +5").is_ok());
    }

    #[test]
    fn expression_led_statements_share_the_expression_grammar() {
        let program = parse("expr.bpfman", "($a or $b) and $c\nnot $a == $b")
            .unwrap_or_else(|_| unreachable!());
        assert!(
            matches!(program.statements()[0].kind(), StatementKind::Expression(value) if matches!(value.kind(), Kind::Logical { operator: LogicalOperator::And, .. }))
        );
        assert!(
            matches!(program.statements()[1].kind(), StatementKind::Expression(value) if matches!(value.kind(), Kind::Not(_)))
        );
    }
}
