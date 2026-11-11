//! Recursive statement collection and block forms. No evaluation occurs here.

use bpfman_shell_source::Span;

use crate::ast::{Identifier, IfBranch, LoopBindings, Statement, StatementKind};
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

fn span(start: &Span, end: &Span) -> Result<Span, SyntaxError> {
    Span::new(start.start().clone(), end.end().clone())
        .map_err(|_| invalid(start, "source span overflow"))
}

#[cfg(test)]
mod tests {
    use crate::ast::{ExpressionKind, StatementKind};
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
}
