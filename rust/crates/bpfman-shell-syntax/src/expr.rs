//! Expression lowering. This module owns precedence and consumes no I/O.

use bpfman_shell_source::{Position, SourceText, Span};

use crate::ast::{
    BinaryOperator, Expression, ExpressionKind, Identifier, InterpolatedPart, LogicalOperator,
    MatchEntry, MatchPattern, MatchPredicate, MatchesBlock, PureBuiltin, RecordField, ThreadCall,
};
use crate::parse::{SyntaxError, invalid};
use crate::token::{Segment, Token, TokenKind, tokenise, tokenise_at};

pub(crate) fn parse_expression(
    tokens: &[Token],
    location: &Span,
) -> Result<Expression, SyntaxError> {
    let mut brace_depth = 0usize;
    let tokens = tokens
        .iter()
        .filter_map(|token| {
            let keep = brace_depth > 0 || !matches!(token.kind, TokenKind::Separator(_));
            if word(token, "{") {
                brace_depth += 1;
            }
            if word(token, "}") {
                brace_depth = brace_depth.saturating_sub(1);
            }
            keep.then(|| token.clone())
        })
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
    PureBuiltin::from_name(text).map(|builtin| (builtin, builtin.arity()))
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
    if starts_numeric && !is_json_number(text) {
        return Err(invalid(
            &token.span,
            format!("invalid numeric literal {text:?}"),
        ));
    }
    Ok(())
}

/// Whether text is exactly one finite JSON number, preserving large integers.
#[must_use]
pub fn is_json_number(text: &str) -> bool {
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

    fn skip_newlines(&mut self) {
        while matches!(
            self.peek().map(|t| &t.kind),
            Some(TokenKind::Separator('\n'))
        ) {
            self.pos += 1;
        }
    }

    fn skip_record_separators(&mut self) {
        while matches!(self.peek().map(|t| &t.kind), Some(TokenKind::Separator(_))) {
            self.pos += 1;
        }
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
            let exhaustive = if self.at("exhaustive") {
                self.take();
                true
            } else {
                false
            };
            let block = self.matches_block(&token.span, exhaustive)?;
            let extent = span(lhs.span(), &block.span)?;
            return Ok(Expression::new(
                ExpressionKind::Matches {
                    target: Box::new(lhs),
                    block,
                },
                extent,
            ));
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
            return self.record();
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
            self.skip_newlines();
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

    fn record(&mut self) -> Result<Expression, SyntaxError> {
        let keyword = self
            .take()
            .ok_or_else(|| invalid(&self.tokens[0].span, "expected record"))?;
        self.skip_record_separators();
        if !self.at("{") {
            return Err(invalid(&keyword.span, "record literal requires '{'"));
        }
        let open = self
            .take()
            .ok_or_else(|| invalid(&keyword.span, "expected '{'"))?;
        let mut fields: Vec<RecordField> = Vec::new();
        loop {
            self.skip_record_separators();
            let Some(token) = self.peek().cloned() else {
                return Err(invalid(&open.span, "missing '}' to close record literal"));
            };
            if word(&token, "}") {
                self.take();
                return Ok(Expression::new(
                    ExpressionKind::Record(fields),
                    span(&keyword.span, &token.span)?,
                ));
            }
            let TokenKind::Word(text) = &token.kind else {
                return Err(invalid(
                    &token.span,
                    "record field must be written as name:",
                ));
            };
            if text.contains(',') {
                return Err(invalid(
                    &token.span,
                    "record fields are whitespace-separated; commas are not record separators",
                ));
            }
            let Some(name_text) = text.strip_suffix(':') else {
                return Err(invalid(
                    &token.span,
                    "record field must be written as name:",
                ));
            };
            let name =
                Identifier::new(name_text.to_owned(), token.span.clone()).ok_or_else(|| {
                    invalid(
                        &token.span,
                        format!("invalid record field name {name_text:?}"),
                    )
                })?;
            if fields.iter().any(|field| field.name.text() == name_text) {
                return Err(invalid(
                    &token.span,
                    format!("duplicate record field {name_text:?}"),
                ));
            }
            self.take();
            let Some(next) = self.peek() else {
                return Err(invalid(
                    &token.span,
                    format!("record field {name_text:?} requires a value"),
                ));
            };
            if word(next, "}") || matches!(next.kind, TokenKind::Separator(_)) {
                return Err(invalid(
                    &token.span,
                    format!("record field {name_text:?} requires a value"),
                ));
            }
            let value = self.term()?;
            fields.push(RecordField {
                name,
                span: span(&token.span, value.span())?,
                value,
            });
        }
    }

    fn matches_block(
        &mut self,
        keyword: &Span,
        exhaustive: bool,
    ) -> Result<MatchesBlock, SyntaxError> {
        if !self.at("{") {
            return Err(invalid(
                keyword,
                "matches requires a block: matches { ... }",
            ));
        }
        let open = self
            .take()
            .ok_or_else(|| invalid(keyword, "expected '{'"))?;
        let mut entries = Vec::new();
        loop {
            self.skip_newlines();
            let Some(token) = self.peek().cloned() else {
                return Err(invalid(
                    &open.span,
                    "unterminated matches block: missing '}'",
                ));
            };
            if word(&token, "}") {
                self.take();
                return Ok(MatchesBlock {
                    entries,
                    exhaustive,
                    span: span(&open.span, &token.span)?,
                });
            }
            if matches!(token.kind, TokenKind::Separator(';')) {
                return Err(invalid(
                    &token.span,
                    "matches: ';' is not a valid entry separator; entries are separated by newlines",
                ));
            }
            if word(&token, ",") {
                return Err(invalid(
                    &token.span,
                    "matches: ',' is not a valid entry separator; entries are separated by newlines",
                ));
            }
            let (path, path_span, leading_pattern) = self.match_path()?;
            if exhaustive && (path.contains('.') || path.contains('[')) {
                return Err(invalid(
                    &path_span,
                    "matches exhaustive: dotted or indexed path is not allowed; use a nested matches block",
                ));
            }
            let Some(next) = leading_pattern.clone().or_else(|| self.peek().cloned()) else {
                return Err(invalid(
                    &path_span,
                    "matches entry: missing pattern after ':'",
                ));
            };
            let pattern = if word(&next, "matches") {
                if leading_pattern.is_none() {
                    self.take();
                }
                let nested_exhaustive = if self.at("exhaustive") {
                    self.take();
                    true
                } else {
                    false
                };
                MatchPattern::Nested(self.matches_block(&next.span, nested_exhaustive)?)
            } else {
                let start = self.pos;
                while let Some(t) = self.peek() {
                    if matches!(t.kind, TokenKind::Separator('\n')) || word(t, "}") {
                        break;
                    }
                    if matches!(t.kind, TokenKind::Separator(';')) {
                        return Err(invalid(
                            &t.span,
                            "matches: ';' is not a valid entry separator; entries are separated by newlines",
                        ));
                    }
                    if let TokenKind::Word(text) = &t.kind {
                        if text.contains(',') {
                            return Err(invalid(
                                &t.span,
                                "matches: ',' is not a valid entry separator; entries are separated by newlines",
                            ));
                        }
                    }
                    self.pos += 1;
                }
                let mut tokens = Vec::new();
                if let Some(token) = leading_pattern {
                    tokens.push(token);
                }
                tokens.extend_from_slice(&self.tokens[start..self.pos]);
                if tokens.is_empty() {
                    return Err(invalid(
                        &path_span,
                        "matches entry: missing pattern after ':'",
                    ));
                }
                match tokens.as_slice() {
                    [
                        Token {
                            kind: TokenKind::Word(text),
                            ..
                        },
                    ] if matches!(text.as_str(), "not-empty" | "null" | "empty") => {
                        MatchPattern::Predicate(match text.as_str() {
                            "not-empty" => MatchPredicate::NotEmpty,
                            "null" => MatchPredicate::Null,
                            _ => MatchPredicate::Empty,
                        })
                    }
                    _ => MatchPattern::Value(parse_expression(&tokens, &path_span)?),
                }
            };
            let end = &self.tokens[self.pos - 1].span;
            entries.push(MatchEntry {
                path,
                pattern,
                span: span(&path_span, end)?,
            });
        }
    }

    fn match_path(&mut self) -> Result<(String, Span, Option<Token>), SyntaxError> {
        let first = self
            .peek()
            .cloned()
            .ok_or_else(|| invalid(&self.tokens[0].span, "matches entry: missing path"))?;
        let TokenKind::Word(text) = &first.kind else {
            return Err(invalid(&first.span, "matches entry: path must be a word"));
        };
        if text.starts_with(':') {
            return Err(invalid(
                &first.span,
                "matches entry: missing path before ':'",
            ));
        }
        let mut path = text.clone();
        self.pos += 1;
        let mut colon = path.ends_with(':');
        let mut leading_pattern = None;
        if colon {
            path.pop();
        }
        while !colon {
            let Some(next) = self.peek() else {
                break;
            };
            let TokenKind::Word(word) = &next.kind else {
                break;
            };
            if word == ":" {
                self.pos += 1;
                colon = true;
            } else if let Some(rest) = word.strip_prefix(':') {
                if !rest.is_empty() {
                    let mut token = next.clone();
                    token.kind = TokenKind::Word(rest.to_owned());
                    let column = next
                        .span
                        .start()
                        .column()
                        .checked_add(1)
                        .ok_or_else(|| invalid(&next.span, "source position overflow"))?;
                    let start = Position::new(
                        next.span.start().name().clone(),
                        next.span.start().line(),
                        column,
                    )
                    .ok_or_else(|| invalid(&next.span, "source position overflow"))?;
                    token.span = Span::new(start, next.span.end().clone())
                        .map_err(|_| invalid(&next.span, "source span overflow"))?;
                    leading_pattern = Some(token);
                }
                self.pos += 1;
                colon = true;
            } else if word == "[" {
                self.pos += 1;
                let index = self
                    .peek()
                    .ok_or_else(|| invalid(&first.span, "matches entry: invalid index"))?;
                let TokenKind::Word(digits) = &index.kind else {
                    return Err(invalid(&index.span, "matches entry: invalid index"));
                };
                path.push('[');
                path.push_str(digits);
                self.pos += 1;
                if !self.at("]") {
                    return Err(invalid(&first.span, "matches entry: invalid index"));
                }
                self.pos += 1;
                path.push(']');
            } else if word.starts_with('.') {
                let part = word.trim_end_matches(':');
                path.push_str(part);
                colon = word.ends_with(':');
                self.pos += 1;
            } else {
                break;
            }
        }
        if !colon {
            return Err(invalid(
                &first.span,
                "matches entry: missing ':' between path and pattern",
            ));
        }
        if !valid_path(&path) {
            return Err(invalid(
                &first.span,
                format!("matches entry: invalid path {path:?}"),
            ));
        }
        Ok((path, first.span, leading_pattern))
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

fn valid_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    let mut pos = 0;
    let mut steps = 0;
    while pos < bytes.len() {
        if bytes[pos] == b'.' {
            pos += 1;
            if pos == bytes.len() {
                break;
            }
        }
        if bytes[pos] == b'[' {
            pos += 1;
            let start = pos;
            while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                pos += 1;
            }
            if pos == start
                || bytes.get(pos) != Some(&b']')
                || path[start..pos].parse::<usize>().is_err()
            {
                return false;
            }
            pos += 1;
        } else {
            let start = pos;
            while pos < bytes.len() && bytes[pos] != b'.' && bytes[pos] != b'[' {
                pos += 1;
            }
            if pos == start {
                return false;
            }
        }
        steps += 1;
    }
    steps > 0
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
    fn invalid_forms_are_rejected() {
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
    fn json_number_validator_is_reusable_by_typed_call_checks() {
        for number in [
            "0",
            "-5",
            "3.14",
            "1e3",
            "1E-2",
            "4026531840",
            "123456789012345678901234567890",
        ] {
            assert!(super::is_json_number(number), "{number}");
        }
        for text in [
            "NaN", "Inf", "+Inf", "-Inf", "Infinity", "+5", "05", "0x10", "1e309", "1 2", "",
            "abc", "1.10.3",
        ] {
            assert!(!super::is_json_number(text), "{text}");
        }
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
