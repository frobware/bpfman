//! A typed renderer of the shell's public AST tree format.

use std::fmt::Write as _;

use bpfman_shell_source::{Position, Span};

use crate::{
    AssertionClause, AssertionMode, BinaryOperator, BindMode, Command, CommandOutcome, Expression,
    ExpressionKind, Identifier, InterpolatedPart, LogicalOperator, LoopBindings, MatchPattern,
    MatchPredicate, MatchesBlock, ParameterType, PositiveDuration, PureBuiltin, Statement,
    StatementKind,
};

/// Render statements as the shell's AST tree using the main source extent.
#[must_use]
pub fn dump_ast(statements: &[Statement], span: Option<&Span>) -> String {
    let mut dumper = Dumper::default();
    dumper.structure("", "Program", |out| {
        out.statements("Stmts", statements);
        if let Some(span) = span {
            out.span(span);
        }
    });
    dumper.text
}

#[derive(Default)]
struct Dumper {
    text: String,
    depth: usize,
}

impl Dumper {
    fn line(&mut self, label: &str, value: &str) {
        for _ in 0..self.depth {
            self.text.push_str("  ");
        }
        if !label.is_empty() {
            self.text.push_str(label);
            self.text.push_str(": ");
        }
        self.text.push_str(value);
        self.text.push('\n');
    }

    fn structure(&mut self, label: &str, name: &str, write: impl FnOnce(&mut Self)) {
        self.line(label, &format!("{name} {{"));
        self.depth += 1;
        write(self);
        self.depth -= 1;
        self.line("", "}");
    }

    fn list<T>(
        &mut self,
        label: &str,
        item: &str,
        values: &[T],
        mut write: impl FnMut(&mut Self, &str, &T),
    ) {
        if !values.is_empty() {
            self.structure(
                label,
                &format!("[]{item} (len = {})", values.len()),
                |out| {
                    for (index, value) in values.iter().enumerate() {
                        write(out, &format!("[{index}]"), value);
                    }
                },
            );
        }
    }

    fn text(&mut self, label: &str, value: &str) {
        if !value.is_empty() {
            self.line(label, &quoted(value));
        }
    }

    fn flag(&mut self, label: &str, value: bool) {
        if value {
            self.line(label, "true");
        }
    }

    fn span(&mut self, span: &Span) {
        self.line(
            "Span",
            &format!("{}-{}", coordinate(span.start()), coordinate(span.end())),
        );
    }

    fn ident(&mut self, label: &str, ident: &Identifier) {
        self.structure(label, "Ident", |out| {
            out.text("Text", ident.text());
            out.span(ident.span());
        });
    }

    fn statements(&mut self, label: &str, statements: &[Statement]) {
        self.list(label, "syntax.Stmt", statements, Self::statement);
    }

    fn statement(&mut self, label: &str, statement: &Statement) {
        let kind = statement.kind();
        let node = match kind {
            StatementKind::Command(_) => "CommandStmt",
            StatementKind::Expression(_) => "ExprStmt",
            StatementKind::Definition(_) => "DefStmt",
            StatementKind::Assertion { .. } => "AssertStmt",
            StatementKind::If { .. } => "IfStmt",
            StatementKind::Foreach { .. } => "ForEachStmt",
            StatementKind::Poll { .. } => "PollStmt",
            StatementKind::Retry { .. } => "RetryStmt",
            StatementKind::Let { .. } => "LetStmt",
            StatementKind::LetDestructure { .. } => "LetDestructureStmt",
            StatementKind::Bind { .. } | StatementKind::BindCollect { .. } => "BindStmt",
            StatementKind::Defer(_) => "DeferStmt",
            StatementKind::Return(_) => "ReturnStmt",
            StatementKind::Break => "BreakStmt",
            StatementKind::Continue => "ContinueStmt",
        };
        self.structure(label, node, |out| {
            match kind {
                StatementKind::Break | StatementKind::Continue => {}
                StatementKind::Command(command) => {
                    out.args(Some(command.head()), command.arguments())
                }
                StatementKind::Expression(value) | StatementKind::Return(value) => {
                    out.expression("Expr", value)
                }
                StatementKind::Definition(definition) => {
                    out.ident("Name", definition.name());
                    out.list(
                        "Params",
                        "syntax.DefParam",
                        definition.parameters(),
                        |out, label, parameter| {
                            out.structure(label, "DefParam", |out| {
                                out.ident("Name", parameter.name());
                                if let Some(annotation) = parameter.annotation() {
                                    out.text(
                                        "Type",
                                        match annotation {
                                            ParameterType::Number => "number",
                                            ParameterType::String => "string",
                                            ParameterType::Bool => "bool",
                                        },
                                    );
                                }
                            });
                        },
                    );
                    out.statements("Body", definition.body());
                }
                StatementKind::Assertion { mode, clause } => {
                    out.flag("IsRequire", *mode == AssertionMode::Require);
                    match clause {
                        AssertionClause::Expression(expression) => {
                            out.structure("Clause", "AssertExprClause", |out| {
                                out.expression("Expr", expression);
                            })
                        }
                        AssertionClause::CommandStatus {
                            outcome,
                            head_span,
                            arguments,
                            negated,
                        } => {
                            out.structure("Clause", "AssertCommandClause", |out| {
                                out.text(
                                    "Head",
                                    if *outcome == CommandOutcome::Ok {
                                        "ok"
                                    } else {
                                        "fail"
                                    },
                                );
                                out.span(head_span);
                                out.args(None, arguments);
                                out.flag("Negate", *negated);
                            });
                        }
                    }
                }
                StatementKind::If {
                    first,
                    elif,
                    else_body,
                } => {
                    out.expression("Cond", first.condition());
                    out.statements("Then", first.body());
                    out.list("Elifs", "syntax.IfBranch", elif, |out, label, branch| {
                        out.structure(label, "IfBranch", |out| {
                            out.expression("Cond", branch.condition());
                            out.statements("Body", branch.body());
                            out.span(branch.span());
                        });
                    });
                    if let Some(body) = else_body {
                        out.statements("Else", body);
                    }
                }
                StatementKind::Foreach {
                    bindings,
                    list,
                    body,
                } => out.foreach(bindings, list, body),
                StatementKind::Poll {
                    timeout,
                    every,
                    body,
                } => {
                    out.line("Timeout", &duration(*timeout));
                    out.line("Every", &duration(*every));
                    out.statements("Body", body);
                }
                StatementKind::Retry { message, unless } => {
                    if let Some(message) = message {
                        out.expression("Message", message);
                    }
                    if let Some(unless) = unless {
                        out.expression("Unless", unless);
                    }
                }
                StatementKind::Let { name, value } => {
                    out.ident("Name", name);
                    out.expression("RHS", value);
                }
                StatementKind::LetDestructure { names, value } => {
                    out.list("Names", "syntax.Ident", names, Self::ident);
                    out.expression("RHS", value);
                }
                StatementKind::Bind {
                    mode,
                    name,
                    command,
                } => {
                    out.ident("Target", name);
                    out.command("Cmd", command);
                    out.flag("Guard", *mode == BindMode::Guard);
                }
                StatementKind::BindCollect {
                    foreach_span,
                    mode,
                    name,
                    bindings,
                    list,
                    body,
                } => {
                    out.ident("Target", name);
                    out.structure("Collect", "ForEachStmt", |out| {
                        out.foreach(bindings, list, body);
                        out.span(foreach_span);
                    });
                    out.flag("Guard", *mode == BindMode::Guard);
                }
                StatementKind::Defer(command) => out.command("Cmd", command),
            }
            out.span(statement.span());
        });
    }

    fn args(&mut self, head: Option<&Expression>, arguments: &[Expression]) {
        let elements = head.into_iter().chain(arguments).collect::<Vec<_>>();
        self.list(
            "Args",
            "syntax.Expr",
            &elements,
            |out, label, expression| out.expression(label, expression),
        );
    }

    fn command(&mut self, label: &str, command: &Command) {
        self.structure(label, "CommandStmt", |out| {
            out.args(Some(command.head()), command.arguments());
            out.span(command.span());
        });
    }

    fn foreach(&mut self, bindings: &LoopBindings, list: &Expression, body: &[Statement]) {
        self.list("Names", "syntax.Ident", bindings.names(), Self::ident);
        self.expression("List", list);
        self.statements("Body", body);
    }

    fn expression(&mut self, label: &str, expression: &Expression) {
        let kind = expression.kind();
        let node = match kind {
            ExpressionKind::Word(_) | ExpressionKind::Quoted(_) => "LiteralExpr",
            ExpressionKind::Variable(_) => "VarRefExpr",
            ExpressionKind::Adapter { .. } => "AdapterExpr",
            ExpressionKind::Interpolated(_) => "InterpStringExpr",
            ExpressionKind::Binary { .. } => "BinaryExpr",
            ExpressionKind::Logical { .. } => "LogicalExpr",
            ExpressionKind::Not(_) => "NotExpr",
            ExpressionKind::Negate(_) => "NegateExpr",
            ExpressionKind::NotEmpty(_) => "UnaryExpr",
            ExpressionKind::Thread { .. } => "ThreadExpr",
            ExpressionKind::PureCall { .. } => "PureCallExpr",
            ExpressionKind::List(_) => "ListExpr",
            ExpressionKind::Record(_) => "RecordExpr",
            ExpressionKind::Matches { .. } => "MatchesExpr",
        };
        self.structure(label, node, |out| {
            match kind {
                ExpressionKind::Word(text) | ExpressionKind::Quoted(text) => {
                    out.text("Text", text);
                    out.flag("Quoted", matches!(kind, ExpressionKind::Quoted(_)));
                }
                ExpressionKind::Variable(variable) => {
                    out.text("Name", &variable.name);
                    out.text("Path", &variable.path);
                }
                ExpressionKind::Adapter { name, variable } => {
                    out.text("Adapter", name);
                    out.text("Name", &variable.name);
                    out.text("Path", &variable.path);
                }
                ExpressionKind::Interpolated(parts) => out.list(
                    "Segments",
                    "syntax.InterpStringSegment",
                    parts,
                    |out, label, part| {
                        out.structure(label, "InterpStringSegment", |out| match part {
                            InterpolatedPart::Literal(text) => out.text("Literal", text),
                            InterpolatedPart::Expression(expression) => {
                                out.expression("Expr", expression)
                            }
                        });
                    },
                ),
                ExpressionKind::Binary {
                    left,
                    operator,
                    right,
                } => {
                    out.expression("Left", left);
                    out.text("Op", binary(*operator));
                    out.expression("Right", right);
                }
                ExpressionKind::Logical {
                    left,
                    operator,
                    right,
                } => {
                    out.text("Op", logical(*operator));
                    out.expression("Left", left);
                    out.expression("Right", right);
                }
                ExpressionKind::Not(operand) | ExpressionKind::Negate(operand) => {
                    out.expression("Operand", operand)
                }
                ExpressionKind::NotEmpty(operand) => {
                    out.text("Pred", "not-empty");
                    out.expression("Operand", operand);
                }
                ExpressionKind::Thread {
                    value,
                    command,
                    pipe,
                } => {
                    out.expression("LHS", value);
                    out.args(Some(command.head()), command.arguments());
                    out.line("PipePos", &coordinate(pipe));
                }
                ExpressionKind::PureCall {
                    builtin: function,
                    arguments,
                } => {
                    out.text("Name", builtin(*function));
                    out.args(None, arguments);
                }
                ExpressionKind::List(values) => {
                    out.list("Elems", "syntax.Expr", values, Self::expression)
                }
                ExpressionKind::Record(fields) => out.list(
                    "Fields",
                    "syntax.RecordField",
                    fields,
                    |out, label, field| {
                        out.structure(label, "RecordField", |out| {
                            out.text("Name", field.name.text());
                            out.expression("Expr", &field.value);
                            out.span(&field.span);
                        });
                    },
                ),
                ExpressionKind::Matches { target, block } => {
                    out.expression("Target", target);
                    out.matches("Block", block);
                }
            }
            out.span(expression.span());
        });
    }

    fn matches(&mut self, label: &str, block: &MatchesBlock) {
        self.structure(label, "MatchesBlockExpr", |out| {
            out.list(
                "Entries",
                "syntax.MatchEntry",
                &block.entries,
                |out, label, entry| {
                    out.structure(label, "MatchEntry", |out| {
                        out.text("Path", &entry.path);
                        match &entry.pattern {
                            MatchPattern::Value(expression) => {
                                out.expression("Pattern", expression)
                            }
                            MatchPattern::Nested(block) => out.matches("SubBlock", block),
                            MatchPattern::Predicate(predicate) => out.text(
                                "Predicate",
                                match predicate {
                                    MatchPredicate::NotEmpty => "not-empty",
                                    MatchPredicate::Null => "null",
                                    MatchPredicate::Empty => "empty",
                                },
                            ),
                        }
                        out.span(&entry.span);
                    });
                },
            );
            out.flag("Exhaustive", block.exhaustive);
            out.span(&block.span);
        });
    }
}

fn coordinate(position: &Position) -> String {
    format!("{}:{}", position.line(), position.column())
}

pub(crate) const fn binary(operator: BinaryOperator) -> &'static str {
    match operator {
        BinaryOperator::Equal => "==",
        BinaryOperator::NotEqual => "!=",
        BinaryOperator::Less => "<",
        BinaryOperator::LessEqual => "<=",
        BinaryOperator::Greater => ">",
        BinaryOperator::GreaterEqual => ">=",
        BinaryOperator::Add => "+",
        BinaryOperator::Subtract => "-",
        BinaryOperator::Multiply => "*",
        BinaryOperator::Divide => "/",
        BinaryOperator::Remainder => "%",
    }
}

pub(crate) const fn logical(operator: LogicalOperator) -> &'static str {
    match operator {
        LogicalOperator::And => "and",
        LogicalOperator::Or => "or",
    }
}

pub(crate) const fn builtin(builtin: PureBuiltin) -> &'static str {
    match builtin {
        PureBuiltin::Jq => "jq",
        PureBuiltin::U32Le => "u32le",
        PureBuiltin::U64Le => "u64le",
        PureBuiltin::Range => "range",
        PureBuiltin::Zip => "zip",
        PureBuiltin::PathExists => "path-exists",
        PureBuiltin::Contains => "contains",
        PureBuiltin::Null => "null",
        PureBuiltin::Present => "present",
        PureBuiltin::Missing => "missing",
        PureBuiltin::Empty => "empty",
    }
}

pub(crate) fn duration(duration: PositiveDuration) -> String {
    let nanos = duration.as_nanos();
    match nanos {
        0..1_000 => format!("{nanos}ns"),
        1_000..1_000_000 => fractional(nanos / 1_000, nanos % 1_000, 3, "µs"),
        1_000_000..1_000_000_000 => fractional(nanos / 1_000_000, nanos % 1_000_000, 6, "ms"),
        _ => {
            let seconds = nanos / 1_000_000_000;
            let mut out = String::new();
            if seconds >= 3600 {
                let _ = write!(out, "{}h", seconds / 3600);
            }
            if seconds >= 60 {
                let _ = write!(out, "{}m", (seconds / 60) % 60);
            }
            out.push_str(&fractional(seconds % 60, nanos % 1_000_000_000, 9, "s"));
            out
        }
    }
}

fn fractional(integer: u64, fraction: u64, digits: usize, unit: &str) -> String {
    if fraction == 0 {
        return format!("{integer}{unit}");
    }
    let fraction = format!("{fraction:0digits$}");
    format!("{integer}.{}{unit}", fraction.trim_end_matches('0'))
}

pub(crate) fn quoted(value: &str) -> String {
    let mut out = String::from("\"");
    for character in value.chars() {
        let escape = match character {
            '"' => "\\\"",
            '\\' => "\\\\",
            '\u{7}' => "\\a",
            '\u{8}' => "\\b",
            '\u{b}' => "\\v",
            '\u{c}' => "\\f",
            '\n' => "\\n",
            '\r' => "\\r",
            '\t' => "\\t",
            character if character.is_ascii_control() => {
                let _ = write!(out, "\\x{:02x}", u32::from(character));
                continue;
            }
            character if character.is_control() => {
                let _ = write!(out, "\\u{:04x}", u32::from(character));
                continue;
            }
            character => {
                out.push(character);
                continue;
            }
        };
        out.push_str(escape);
    }
    out.push('"');
    out
}
