//! Recursive statement collection and block forms. No evaluation occurs here.

use bpfman_shell_source::Span;

use crate::ast::{
    Definition, Identifier, IfBranch, LoopBindings, Parameter, ParameterType, Statement,
    StatementKind,
};
use crate::duration::{PositiveDuration, parse_positive_duration};
use crate::expr::parse_expression;
use crate::parse::{SyntaxError, invalid, parse_statement};
use crate::token::{Token, TokenKind};

pub(crate) fn parse_statements(tokens: &[Token]) -> Result<Vec<Statement>, SyntaxError> {
    StatementCursor { tokens, pos: 0 }.statements(false)
}

struct StatementCursor<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl StatementCursor<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn at(&self, text: &str) -> bool {
        self.peek()
            .is_some_and(|token| matches!(&token.kind, TokenKind::Word(word) if word == text))
    }

    fn skip_separators(&mut self) {
        while self
            .peek()
            .is_some_and(|token| matches!(token.kind, TokenKind::Separator(_)))
        {
            self.pos += 1;
        }
    }

    fn statements(&mut self, in_block: bool) -> Result<Vec<Statement>, SyntaxError> {
        let mut statements = Vec::new();
        loop {
            self.skip_separators();
            let Some(token) = self.peek() else { break };
            if self.at("}") {
                if in_block {
                    break;
                }
                return Err(invalid(&token.span, "unexpected '}' at statement start"));
            }
            let statement = if self.at("if") {
                self.if_statement()?
            } else if self.at("foreach") {
                self.foreach_statement()?
            } else if self.at("def") {
                self.definition_statement()?
            } else if self.at("poll") {
                self.poll_statement()?
            } else {
                self.simple_statement()?
            };
            statements.push(statement);
        }
        Ok(statements)
    }

    fn simple_statement(&mut self) -> Result<Statement, SyntaxError> {
        let start = self.pos;
        let mut depth = 0usize;
        let mut collected = Vec::new();
        while let Some(token) = self.peek() {
            match &token.kind {
                TokenKind::Separator(_) if depth == 0 => break,
                TokenKind::Separator(_) => {}
                TokenKind::Word(word) if depth == 0 && (word == "{" || word == "}") => break,
                TokenKind::Word(word) if word == "(" || word == "[" => {
                    depth += 1;
                    collected.push(token.clone());
                }
                TokenKind::Word(word) if (word == ")" || word == "]") && depth > 0 => {
                    depth -= 1;
                    collected.push(token.clone());
                }
                _ => collected.push(token.clone()),
            }
            self.pos += 1;
        }
        if collected.is_empty() {
            return Err(invalid(&self.tokens[start].span, "expected a statement"));
        }
        parse_statement(&collected)
    }

    fn if_statement(&mut self) -> Result<Statement, SyntaxError> {
        let keyword = self.tokens[self.pos].clone();
        self.pos += 1;
        let first = self.if_branch(&keyword)?;
        let mut end = first.span().clone();
        let mut elif = Vec::new();
        let mut else_body = None;
        loop {
            self.skip_separators();
            if self.at("elif") {
                let branch_keyword = self.tokens[self.pos].clone();
                self.pos += 1;
                let branch = self.if_branch(&branch_keyword)?;
                end = branch.span().clone();
                elif.push(branch);
            } else if self.at("else") {
                self.pos += 1;
                let (body, close) = self.block()?;
                end = span(&keyword.span, &close)?;
                else_body = Some(body);
                break;
            } else {
                break;
            }
        }
        let extent = span(&keyword.span, &end)?;
        Ok(Statement::new(
            StatementKind::If {
                first,
                elif,
                else_body,
            },
            extent,
        ))
    }

    fn if_branch(&mut self, keyword: &Token) -> Result<IfBranch, SyntaxError> {
        let condition_tokens =
            self.until_open_brace(&keyword.span, "expected '{' after condition")?;
        if condition_tokens.is_empty() {
            return Err(invalid(&keyword.span, "expected condition before '{'"));
        }
        let condition = parse_expression(&condition_tokens, &keyword.span)?;
        let (body, close) = self.block()?;
        Ok(IfBranch::new(condition, body, span(&keyword.span, &close)?))
    }

    fn foreach_statement(&mut self) -> Result<Statement, SyntaxError> {
        let keyword = self.tokens[self.pos].clone();
        self.pos += 1;
        let bindings = self.loop_bindings(&keyword.span)?;
        if !self.at("in") {
            return Err(invalid(
                &keyword.span,
                "foreach requires 'in' after the loop variable",
            ));
        }
        self.pos += 1;
        let list_tokens = self.until_open_brace(&keyword.span, "expected '{' after expression")?;
        if list_tokens.is_empty() {
            return Err(invalid(&keyword.span, "foreach requires a list expression"));
        }
        let list = parse_expression(&list_tokens, &keyword.span)?;
        let (body, close) = self.block()?;
        let extent = span(&keyword.span, &close)?;
        Ok(Statement::new(
            StatementKind::Foreach {
                bindings,
                list,
                body,
            },
            extent,
        ))
    }

    fn definition_statement(&mut self) -> Result<Statement, SyntaxError> {
        let keyword = self.tokens[self.pos].clone();
        self.pos += 1;
        let Some(name_token) = self.peek() else {
            return Err(invalid(
                &keyword.span,
                "def requires: def <name>(<params>) { ... }",
            ));
        };
        if self.at("(") {
            return Err(invalid(&keyword.span, "def requires a name before '('"));
        }
        let TokenKind::Word(text) = &name_token.kind else {
            return Err(invalid(&name_token.span, "def requires a name"));
        };
        let name = Identifier::new(text.clone(), name_token.span.clone())
            .ok_or_else(|| invalid(&name_token.span, format!("invalid def name: {text:?}")))?;
        if reserved_definition_name(name.text()) {
            return Err(invalid(
                &name_token.span,
                format!("cannot use reserved word {text:?} as a def name"),
            ));
        }
        self.pos += 1;
        if !self.at("(") {
            return Err(invalid(&keyword.span, "def requires '(' after the name"));
        }
        self.pos += 1;
        let parameters = self.definition_parameters(&keyword.span)?;
        self.skip_separators();
        let (body, close) = self.block()?;
        let extent = span(&keyword.span, &close)?;
        Ok(Statement::new(
            StatementKind::Definition(Definition::new(name, parameters, body)),
            extent,
        ))
    }

    fn poll_statement(&mut self) -> Result<Statement, SyntaxError> {
        let keyword = self.tokens[self.pos].clone();
        self.pos += 1;
        if !self.at("timeout") {
            return Err(invalid(
                &keyword.span,
                "poll requires 'timeout DUR every DUR'",
            ));
        }
        self.pos += 1;
        let timeout = self.duration_word(&keyword.span, "timeout")?;
        if !self.at("every") {
            return Err(invalid(
                &keyword.span,
                "poll requires 'every DUR' after timeout",
            ));
        }
        self.pos += 1;
        let every = self.duration_word(&keyword.span, "every")?;
        let (body, close) = self.block()?;
        let extent = span(&keyword.span, &close)?;
        Ok(Statement::new(
            StatementKind::Poll {
                timeout,
                every,
                body,
            },
            extent,
        ))
    }

    fn duration_word(
        &mut self,
        keyword: &Span,
        clause: &str,
    ) -> Result<PositiveDuration, SyntaxError> {
        let Some(token) = self.peek() else {
            return Err(invalid(
                keyword,
                format!("poll {clause} requires a duration literal"),
            ));
        };
        let TokenKind::Word(text) = &token.kind else {
            return Err(invalid(
                keyword,
                format!("poll {clause} requires a duration literal"),
            ));
        };
        let duration = parse_positive_duration(text).ok_or_else(|| {
            invalid(
                &token.span,
                format!("poll {clause}: invalid or nonpositive duration {text:?}"),
            )
        })?;
        self.pos += 1;
        Ok(duration)
    }

    fn definition_parameters(&mut self, keyword: &Span) -> Result<Vec<Parameter>, SyntaxError> {
        let mut parameters = Vec::new();
        loop {
            self.skip_separators();
            let Some(token) = self.peek().cloned() else {
                return Err(invalid(
                    keyword,
                    "def: unterminated parameter list (missing ')')",
                ));
            };
            if self.at(")") {
                self.pos += 1;
                return Ok(parameters);
            }
            let TokenKind::Word(text) = &token.kind else {
                return Err(invalid(&token.span, "def: expected parameter name"));
            };
            if text.contains(',') {
                return Err(invalid(
                    &token.span,
                    "def: comma is not a parameter separator",
                ));
            }
            let annotated = text.contains(':');
            if annotated
                && (!text.ends_with(':') || text.bytes().filter(|byte| *byte == b':').count() != 1)
            {
                return Err(invalid(
                    &token.span,
                    "def: parameter annotation needs a space after the colon",
                ));
            }
            let name_text = text.strip_suffix(':').unwrap_or(text);
            let name =
                Identifier::new(name_text.to_owned(), token.span.clone()).ok_or_else(|| {
                    invalid(
                        &token.span,
                        format!("def: invalid parameter name {name_text:?}"),
                    )
                })?;
            if name.text() == "_" {
                return Err(invalid(&token.span, "def parameters cannot bind '_'"));
            }
            if parameters
                .iter()
                .any(|param: &Parameter| param.name().text() == name.text())
            {
                return Err(invalid(
                    &token.span,
                    format!("def: duplicate parameter name {name_text:?}"),
                ));
            }
            self.pos += 1;
            let annotation = if annotated {
                self.skip_separators();
                let Some(type_token) = self.peek() else {
                    return Err(invalid(
                        &token.span,
                        format!("def: parameter {name_text:?}: annotation requires a type"),
                    ));
                };
                let kind = match &type_token.kind {
                    TokenKind::Word(word) if word == "number" => ParameterType::Number,
                    TokenKind::Word(word) if word == "string" => ParameterType::String,
                    TokenKind::Word(word) if word == "bool" => ParameterType::Bool,
                    TokenKind::Word(word) if word == ")" => {
                        return Err(invalid(
                            &token.span,
                            format!("def: parameter {name_text:?}: annotation requires a type"),
                        ));
                    }
                    TokenKind::Word(word) => {
                        return Err(invalid(
                            &type_token.span,
                            format!(
                                "def: parameter {name_text:?}: unknown parameter type {word:?} (expected one of: number, string, bool)"
                            ),
                        ));
                    }
                    _ => return Err(invalid(&type_token.span, "def: annotation requires a type")),
                };
                self.pos += 1;
                Some(kind)
            } else {
                None
            };
            parameters.push(Parameter::new(name, annotation));
        }
    }

    fn loop_bindings(&mut self, keyword: &Span) -> Result<LoopBindings, SyntaxError> {
        if self.at("(") {
            let open = self.tokens[self.pos].clone();
            self.pos += 1;
            let mut names = Vec::new();
            loop {
                self.skip_separators();
                let Some(token) = self.peek() else {
                    return Err(invalid(
                        &open.span,
                        "foreach: unterminated name list (missing ')')",
                    ));
                };
                if self.at(")") {
                    self.pos += 1;
                    break;
                }
                let name = loop_name(token)?;
                if name.text() != "_"
                    && names
                        .iter()
                        .any(|old: &Identifier| old.text() == name.text())
                {
                    return Err(invalid(
                        &token.span,
                        format!("foreach: duplicate name {:?}", name.text()),
                    ));
                }
                names.push(name);
                self.pos += 1;
            }
            if names.len() < 2 {
                return Err(invalid(
                    &open.span,
                    "foreach: parenthesised name list requires at least two names",
                ));
            }
            return LoopBindings::destructured(names)
                .ok_or_else(|| invalid(keyword, "foreach: at least one loop variable must bind"));
        }
        let token = self
            .peek()
            .ok_or_else(|| invalid(keyword, "foreach requires a variable name"))?;
        let name = loop_name(token)?;
        self.pos += 1;
        Ok(LoopBindings::single(name))
    }

    fn until_open_brace(
        &mut self,
        fallback: &Span,
        missing: &str,
    ) -> Result<Vec<Token>, SyntaxError> {
        let mut tokens = Vec::new();
        let mut depth = 0usize;
        while let Some(token) = self.peek() {
            match &token.kind {
                TokenKind::Separator(_) => {
                    self.pos += 1;
                    continue;
                }
                TokenKind::Word(word) if word == "{" && depth == 0 => return Ok(tokens),
                TokenKind::Word(word) if word == "}" && depth == 0 => break,
                TokenKind::Word(word) if word == "(" || word == "[" => depth += 1,
                TokenKind::Word(word) if (word == ")" || word == "]") && depth > 0 => depth -= 1,
                _ => {}
            }
            tokens.push(token.clone());
            self.pos += 1;
        }
        Err(invalid(fallback, missing))
    }

    fn block(&mut self) -> Result<(Vec<Statement>, Span), SyntaxError> {
        let Some(open) = self.peek() else {
            return Err(invalid(&self.tokens[self.pos - 1].span, "expected '{'"));
        };
        if !self.at("{") {
            return Err(invalid(&open.span, "expected '{'"));
        }
        let opening = open.span.clone();
        self.pos += 1;
        let statements = self.statements(true)?;
        let Some(close) = self.peek() else {
            return Err(invalid(&opening, "unterminated block: missing '}'"));
        };
        if !self.at("}") {
            return Err(invalid(&opening, "unterminated block: missing '}'"));
        }
        let close = close.span.clone();
        self.pos += 1;
        Ok((statements, close))
    }
}

fn loop_name(token: &Token) -> Result<Identifier, SyntaxError> {
    let TokenKind::Word(text) = &token.kind else {
        return Err(invalid(&token.span, "foreach requires a variable name"));
    };
    if text.contains(',') {
        return Err(invalid(&token.span, "foreach: comma is not a separator"));
    }
    if text == "in" {
        return Err(invalid(
            &token.span,
            "foreach requires a variable name before 'in'",
        ));
    }
    Identifier::new(text.clone(), token.span.clone())
        .ok_or_else(|| invalid(&token.span, format!("invalid variable name: {text:?}")))
}

fn reserved_definition_name(name: &str) -> bool {
    matches!(
        name,
        "def"
            | "defer"
            | "let"
            | "guard"
            | "if"
            | "elif"
            | "else"
            | "foreach"
            | "in"
            | "retry"
            | "poll"
            | "return"
            | "break"
            | "continue"
            | "assert"
            | "require"
            | "and"
            | "or"
            | "not"
            | "matches"
            | "true"
            | "false"
    )
}

fn span(start: &Span, end: &Span) -> Result<Span, SyntaxError> {
    Span::new(start.start().clone(), end.end().clone())
        .map_err(|_| invalid(start, "source span overflow"))
}

#[cfg(test)]
mod tests {
    use crate::ast::{ExpressionKind, ParameterType, StatementKind};
    use crate::parse::{SyntaxErrorKind, parse};

    #[test]
    fn if_elif_else_preserves_branch_structure_and_spans() {
        let program = parse(
            "blocks.bpfman",
            "if $a { print a }\nelif $b { print b } else { print c }",
        )
        .unwrap_or_else(|_| unreachable!());
        let [statement] = program.statements() else {
            unreachable!()
        };
        let StatementKind::If {
            first,
            elif,
            else_body,
        } = statement.kind()
        else {
            unreachable!()
        };
        assert!(
            matches!(first.condition().kind(), ExpressionKind::Variable(var) if var.name == "a")
        );
        assert_eq!(first.body().len(), 1);
        assert_eq!(elif.len(), 1);
        assert!(
            matches!(elif[0].condition().kind(), ExpressionKind::Variable(var) if var.name == "b")
        );
        assert_eq!(else_body.as_ref().map(Vec::len), Some(1));
        assert_eq!(statement.span().start().line(), 1);
        assert_eq!(statement.span().end().line(), 2);
        assert_eq!(statement.span().end().column(), 37);
    }

    #[test]
    fn nested_blocks_and_following_statements_stay_separate() {
        let program = parse(
            "blocks.bpfman",
            "foreach a in $xs {\n if $a { foreach b in $ys { print b } }\n}\nprint done",
        )
        .unwrap_or_else(|_| unreachable!());
        let [outer, following] = program.statements() else {
            unreachable!()
        };
        let StatementKind::Foreach { body, .. } = outer.kind() else {
            unreachable!()
        };
        let [inside] = body.as_slice() else {
            unreachable!()
        };
        let StatementKind::If { first, .. } = inside.kind() else {
            unreachable!()
        };
        assert!(matches!(
            first.body()[0].kind(),
            StatementKind::Foreach { .. }
        ));
        assert!(matches!(following.kind(), StatementKind::Command(_)));
    }

    #[test]
    fn foreach_names_and_list_expression_match_go_forms() {
        for (source, expected) in [
            ("foreach x in $xs { print x }", vec!["x"]),
            ("foreach _ in $xs { print ok }", vec!["_"]),
            ("foreach (a _ b) in $xs { print a }", vec!["a", "_", "b"]),
            ("foreach (a\n b) in $xs { print a }", vec!["a", "b"]),
        ] {
            let program = parse("blocks.bpfman", source).unwrap_or_else(|_| unreachable!());
            let StatementKind::Foreach { bindings, body, .. } = program.statements()[0].kind()
            else {
                unreachable!()
            };
            assert_eq!(
                bindings
                    .names()
                    .iter()
                    .map(|name| name.text())
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(bindings.is_destructured(), expected.len() > 1);
            assert_eq!(body.len(), 1);
        }
        let program = parse(
            "blocks.bpfman",
            "foreach p in $raw |> jq '.items' { print p }",
        )
        .unwrap_or_else(|_| unreachable!());
        assert!(
            matches!(program.statements()[0].kind(), StatementKind::Foreach { list, .. } if matches!(list.kind(), ExpressionKind::Thread { .. }))
        );
    }

    #[test]
    fn multiline_conditions_and_empty_else_are_distinct() {
        let program = parse("blocks.bpfman", "if ($a\n or $b) { }\nelse {}")
            .unwrap_or_else(|_| unreachable!());
        let StatementKind::If {
            first, else_body, ..
        } = program.statements()[0].kind()
        else {
            unreachable!()
        };
        assert!(matches!(
            first.condition().kind(),
            ExpressionKind::Logical { .. }
        ));
        assert!(first.body().is_empty());
        assert!(else_body.as_ref().is_some_and(Vec::is_empty));
    }

    #[test]
    fn malformed_blocks_and_bindings_fail_locally() {
        for (source, message) in [
            ("if $ready {", "unterminated block"),
            ("if $ready print yes", "expected '{'"),
            ("if { print yes }", "expected condition"),
            ("foreach in $xs { print x }", "variable name"),
            ("foreach (x) in $xs {}", "at least two names"),
            ("foreach (_ _) in $xs {}", "at least one loop variable"),
            ("foreach (x x) in $xs {}", "duplicate name"),
            ("foreach x, y in $xs {}", "comma is not a separator"),
            ("foreach x in $xs {", "unterminated block"),
            ("}", "unexpected '}'"),
        ] {
            let error = parse("blocks.bpfman", source)
                .err()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(error.kind(), SyntaxErrorKind::Invalid, "{source}");
            assert!(error.message().contains(message), "{source}: {error}");
        }
    }

    #[test]
    fn conditional_spans_end_at_the_last_closing_brace() {
        for (source, line, column) in [
            ("if $x {\n  print yes\n}\nprint after", 3, 2),
            (
                "if $x {\n print x\n}\nelif $y {\n print y\n}\nprint after",
                6,
                2,
            ),
            (
                "if $x {\n print x\n}\nelse {\n print y\n}\nprint after",
                6,
                2,
            ),
            (
                "if $outer {\n if $inner {\n  print yes\n }\n}\nprint after",
                5,
                2,
            ),
        ] {
            let program = parse("blocks.bpfman", source).unwrap_or_else(|_| unreachable!());
            assert_eq!(
                program.statements()[0].span().end().line(),
                line,
                "{source}"
            );
            assert_eq!(
                program.statements()[0].span().end().column(),
                column,
                "{source}"
            );
            assert_eq!(program.statements().len(), 2, "{source}");
        }
    }

    #[test]
    fn definitions_parse_typed_parameters_and_nested_bodies() {
        let source = "def greet(name: string count: number enabled: bool extra) {\n if $enabled { print $name }\n return $count\n}\ngreet alice 2 true x";
        let program = parse("blocks.bpfman", source).unwrap_or_else(|_| unreachable!());
        let [definition, call] = program.statements() else {
            unreachable!()
        };
        let StatementKind::Definition(definition) = definition.kind() else {
            unreachable!()
        };
        assert_eq!(definition.name().text(), "greet");
        assert_eq!(definition.parameters().len(), 4);
        assert_eq!(
            definition.parameters()[0].annotation(),
            Some(ParameterType::String)
        );
        assert_eq!(
            definition.parameters()[1].annotation(),
            Some(ParameterType::Number)
        );
        assert_eq!(
            definition.parameters()[2].annotation(),
            Some(ParameterType::Bool)
        );
        assert_eq!(definition.parameters()[3].annotation(), None);
        assert!(matches!(
            definition.body()[0].kind(),
            StatementKind::If { .. }
        ));
        assert!(matches!(
            definition.body()[1].kind(),
            StatementKind::Return(_)
        ));
        assert!(matches!(call.kind(), StatementKind::Command(_)));
        assert_eq!(program.statements()[0].span().end().line(), 4);

        let empty = parse("blocks.bpfman", "def f() {}").unwrap_or_else(|_| unreachable!());
        assert!(
            matches!(empty.statements()[0].kind(), StatementKind::Definition(definition) if definition.parameters().is_empty() && definition.body().is_empty())
        );
    }

    #[test]
    fn definition_parameter_errors_match_go_contracts() {
        for (source, message) in [
            ("def (x) {}", "name before '('"),
            ("def if() {}", "reserved word"),
            ("def f x {}", "requires '('"),
            ("def f(a:number) {}", "space after the colon"),
            ("def f(a:) {}", "annotation requires a type"),
            ("def f(a: integer) {}", "unknown parameter type"),
            ("def f(a a) {}", "duplicate parameter name"),
            ("def f(_ a) {}", "cannot bind '_'"),
            ("def f(a, b) {}", "comma is not a parameter separator"),
            ("def f(a b", "unterminated parameter list"),
        ] {
            let error = parse("blocks.bpfman", source)
                .err()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(error.kind(), SyntaxErrorKind::Invalid, "{source}");
            assert!(error.message().contains(message), "{source}: {error}");
        }
    }

    #[test]
    fn poll_and_retry_keep_timing_and_optional_clauses_typed() {
        let source = "poll timeout 5s every 100ms {\n retry\n retry 'waiting'\n retry unless $ok\n retry 'waiting' unless $ok\n}";
        let program = parse("blocks.bpfman", source).unwrap_or_else(|_| unreachable!());
        let StatementKind::Poll {
            timeout,
            every,
            body,
        } = program.statements()[0].kind()
        else {
            unreachable!()
        };
        assert_eq!(timeout.as_nanos(), 5_000_000_000);
        assert_eq!(every.as_nanos(), 100_000_000);
        assert_eq!(body.len(), 4);
        assert!(matches!(
            body[0].kind(),
            StatementKind::Retry {
                message: None,
                unless: None
            }
        ));
        assert!(matches!(
            body[1].kind(),
            StatementKind::Retry {
                message: Some(_),
                unless: None
            }
        ));
        assert!(matches!(
            body[2].kind(),
            StatementKind::Retry {
                message: None,
                unless: Some(_)
            }
        ));
        assert!(matches!(
            body[3].kind(),
            StatementKind::Retry {
                message: Some(_),
                unless: Some(_)
            }
        ));
    }

    #[test]
    fn poll_rejects_missing_or_nonpositive_durations() {
        for (source, message) in [
            ("poll every 1s {}", "requires 'timeout"),
            ("poll timeout 5s {}", "requires 'every"),
            (
                "poll timeout 0 every 1s {}",
                "invalid or nonpositive duration",
            ),
            (
                "poll timeout 0.1ns every 1s {}",
                "invalid or nonpositive duration",
            ),
            (
                "poll timeout 1s every -1s {}",
                "invalid or nonpositive duration",
            ),
            (
                "poll timeout 1s every 1day {}",
                "invalid or nonpositive duration",
            ),
            ("poll timeout 1s every 1s {", "unterminated block"),
            ("retry unless", "condition expression"),
        ] {
            let error = parse("blocks.bpfman", source)
                .err()
                .unwrap_or_else(|| unreachable!());
            assert_eq!(error.kind(), SyntaxErrorKind::Invalid, "{source}");
            assert!(error.message().contains(message), "{source}: {error}");
        }
    }
}
