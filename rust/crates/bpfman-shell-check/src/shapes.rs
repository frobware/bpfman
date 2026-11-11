//! Conservative shape propagation and proved field-access diagnostics.

use std::collections::{HashMap, HashSet};

use bpfman_shell_semantics::{
    FieldLookup, OriginKind, Shape, bind_primary_shape, collect_shape, kind_shape, outcome_shape,
    pure_shape,
};
use bpfman_shell_source::Span;
use bpfman_shell_syntax::{
    AssertionClause, BinaryOperator, Command, Expression, ExpressionKind, InterpolatedPart,
    MatchPattern, MatchesBlock, Statement, StatementKind, VariableRef,
};

use crate::{ExpandedProgram, Issue};

/// Report field-access mistakes on shapes the checker can prove sealed.
/// Unknown and domain shapes without a verified Rust JSON projection stay open.
/// An empty result does not certify execution.
#[must_use]
pub fn analyze_shapes(program: &ExpandedProgram) -> Vec<Issue> {
    let defs = program
        .statements()
        .iter()
        .filter_map(|statement| match statement.kind() {
            StatementKind::Definition(definition) => Some(definition.name().text().to_owned()),
            _ => None,
        })
        .collect();
    let mut analysis = Analysis {
        frames: vec![HashMap::new()],
        defs,
        issues: Vec::new(),
    };
    analysis.statements(program.statements());
    analysis.issues
}

struct Analysis {
    frames: Vec<HashMap<String, Shape>>,
    defs: HashSet<String>,
    issues: Vec<Issue>,
}

impl Analysis {
    fn issue(&mut self, span: &Span, message: impl Into<String>) {
        self.issues.push(Issue {
            span: span.clone(),
            message: message.into(),
        });
    }

    fn define(&mut self, name: &str, shape: Shape) {
        if name != "_" {
            if let Some(frame) = self.frames.last_mut() {
                frame.insert(name.to_owned(), shape);
            }
        }
    }

    fn lookup(&self, name: &str) -> Option<&Shape> {
        self.frames.iter().rev().find_map(|frame| frame.get(name))
    }

    fn scoped(&mut self, names: impl IntoIterator<Item = String>, body: &[Statement]) {
        self.frames.push(HashMap::new());
        for name in names {
            self.define(&name, Shape::unknown());
        }
        self.statements(body);
        self.frames.pop();
    }

    fn statements(&mut self, statements: &[Statement]) {
        for statement in statements {
            self.statement(statement);
        }
    }

    fn statement(&mut self, statement: &Statement) {
        match statement.kind() {
            StatementKind::Command(command) | StatementKind::Defer(command) => {
                self.command(command)
            }
            StatementKind::Expression(value) | StatementKind::Return(value) => {
                self.expression(value)
            }
            StatementKind::Let { name, value } => {
                self.expression(value);
                let shape = self.infer(value);
                self.define(name.text(), shape);
            }
            StatementKind::LetDestructure { names, value } => {
                self.expression(value);
                for name in names {
                    self.define(name.text(), Shape::unknown());
                }
            }
            StatementKind::Bind {
                mode,
                name,
                command,
            } => {
                self.command(command);
                let primary = if matches!(command.head().kind(), ExpressionKind::Word(head) if self.defs.contains(head))
                {
                    Shape::unknown()
                } else {
                    bind_primary_shape(command)
                };
                let shape = if *mode == bpfman_shell_syntax::BindMode::Guard {
                    primary
                } else {
                    outcome_shape(primary)
                };
                self.define(name.text(), shape);
            }
            StatementKind::BindCollect {
                mode,
                name,
                bindings,
                list,
                body,
                ..
            } => {
                self.expression(list);
                self.scoped(
                    bindings.names().iter().map(|name| name.text().to_owned()),
                    body,
                );
                let primary = match body.last().map(Statement::kind) {
                    Some(StatementKind::Command(command)) if matches!(command.head().kind(), ExpressionKind::Word(head) if self.defs.contains(head)) => {
                        Shape::unknown()
                    }
                    Some(StatementKind::Command(command)) => bind_primary_shape(command),
                    _ => Shape::unknown(),
                };
                self.define(
                    name.text(),
                    collect_shape(primary, *mode == bpfman_shell_syntax::BindMode::Guard),
                );
            }
            StatementKind::Definition(definition) => {
                self.scoped(
                    definition
                        .parameters()
                        .iter()
                        .map(|param| param.name().text().to_owned()),
                    definition.body(),
                );
            }
            StatementKind::If {
                first,
                elif,
                else_body,
            } => {
                self.expression(first.condition());
                self.scoped([], first.body());
                for branch in elif {
                    self.expression(branch.condition());
                    self.scoped([], branch.body());
                }
                if let Some(body) = else_body {
                    self.scoped([], body);
                }
            }
            StatementKind::Foreach {
                bindings,
                list,
                body,
            } => {
                self.expression(list);
                self.scoped(
                    bindings.names().iter().map(|name| name.text().to_owned()),
                    body,
                );
            }
            StatementKind::Poll { body, .. } => self.scoped([], body),
            StatementKind::Retry { message, unless } => {
                if let Some(value) = message {
                    self.expression(value);
                }
                if let Some(value) = unless {
                    self.expression(value);
                }
            }
            StatementKind::Assertion { clause, .. } => match clause {
                AssertionClause::Expression(value) => self.expression(value),
                AssertionClause::CommandStatus { arguments, .. } => {
                    for value in arguments {
                        self.expression(value);
                    }
                }
            },
            StatementKind::Break | StatementKind::Continue => {}
        }
    }

    fn command(&mut self, command: &Command) {
        self.expression(command.head());
        for argument in command.arguments() {
            self.expression(argument);
        }
    }

    fn expression(&mut self, value: &Expression) {
        match value.kind() {
            ExpressionKind::Variable(variable) | ExpressionKind::Adapter { variable, .. } => {
                self.path_shape(variable, value.span());
            }
            ExpressionKind::Binary { left, right, .. }
            | ExpressionKind::Logical { left, right, .. } => {
                self.expression(left);
                self.expression(right);
            }
            ExpressionKind::Not(inner)
            | ExpressionKind::Negate(inner)
            | ExpressionKind::NotEmpty(inner) => self.expression(inner),
            ExpressionKind::Thread { value, command, .. } => {
                self.expression(value);
                self.expression(command.head());
                for argument in command.arguments() {
                    self.expression(argument);
                }
            }
            ExpressionKind::PureCall { arguments, .. } | ExpressionKind::List(arguments) => {
                for argument in arguments {
                    self.expression(argument);
                }
            }
            ExpressionKind::Record(fields) => {
                for field in fields {
                    self.expression(&field.value);
                }
            }
            ExpressionKind::Matches { target, block } => {
                self.expression(target);
                self.matches_block(block);
            }
            ExpressionKind::Interpolated(parts) => {
                for part in parts {
                    if let InterpolatedPart::Expression(value) = part {
                        self.expression(value);
                    }
                }
            }
            ExpressionKind::Word(_) | ExpressionKind::Quoted(_) => {}
        }
    }

    fn matches_block(&mut self, block: &MatchesBlock) {
        for entry in &block.entries {
            match &entry.pattern {
                MatchPattern::Value(value) => self.expression(value),
                MatchPattern::Nested(block) => self.matches_block(block),
                MatchPattern::Predicate(_) => {}
            }
        }
    }

    fn infer(&self, value: &Expression) -> Shape {
        use OriginKind as K;
        match value.kind() {
            ExpressionKind::Word(text) => match text.as_str() {
                "true" | "false" => kind_shape(K::Bool),
                "null" => kind_shape(K::Null),
                _ => kind_shape(K::Scalar),
            },
            ExpressionKind::Quoted(_)
            | ExpressionKind::Interpolated(_)
            | ExpressionKind::Negate(_) => kind_shape(K::Scalar),
            ExpressionKind::Variable(variable) => self.path_shape_readonly(variable),
            ExpressionKind::Adapter { .. } => Shape::unknown(),
            ExpressionKind::Binary { operator, .. } => match operator {
                BinaryOperator::Add
                | BinaryOperator::Subtract
                | BinaryOperator::Multiply
                | BinaryOperator::Divide
                | BinaryOperator::Remainder => kind_shape(K::Scalar),
                _ => kind_shape(K::Bool),
            },
            ExpressionKind::Logical { .. }
            | ExpressionKind::Not(_)
            | ExpressionKind::NotEmpty(_)
            | ExpressionKind::Matches { .. } => kind_shape(K::Bool),
            ExpressionKind::Thread { .. } => Shape::unknown(),
            ExpressionKind::PureCall { builtin, .. } => pure_shape(*builtin),
            ExpressionKind::List(elements) => {
                let Some(first) = elements.first() else {
                    return Shape::unknown();
                };
                let shape = self.infer(first);
                if shape == Shape::unknown()
                    || elements[1..].iter().any(|value| self.infer(value) != shape)
                {
                    Shape::unknown()
                } else {
                    Shape::list(shape)
                }
            }
            ExpressionKind::Record(fields) => Shape::record(
                K::Unknown,
                fields
                    .iter()
                    .map(|field| (field.name.text().to_owned(), self.infer(&field.value))),
            ),
        }
    }

    fn path_shape_readonly(&self, variable: &VariableRef) -> Shape {
        let Some(mut shape) = self.lookup(&variable.name).cloned() else {
            return Shape::unknown();
        };
        for segment in path_segments(&variable.path) {
            shape = match segment {
                Segment::Index => shape.index(),
                Segment::Field(name) => match shape.field(name) {
                    FieldLookup::Found(child) => child.clone(),
                    _ => return Shape::unknown(),
                },
            };
        }
        shape
    }

    fn path_shape(&mut self, variable: &VariableRef, span: &Span) -> Shape {
        let Some(mut shape) = self.lookup(&variable.name).cloned() else {
            return Shape::unknown();
        };
        let mut current_name = variable.name.clone();
        for segment in path_segments(&variable.path) {
            match segment {
                Segment::Index => shape = shape.index(),
                Segment::Field(name) => match shape.field(name) {
                    FieldLookup::Found(child) => {
                        shape = child.clone();
                        current_name.push('.');
                        current_name.push_str(name);
                    }
                    FieldLookup::Open => return Shape::unknown(),
                    FieldLookup::Leaf(kind) => {
                        self.issue(
                            span,
                            format!("{current_name} has kind {kind}; field access is not valid"),
                        );
                        return Shape::unknown();
                    }
                    FieldLookup::Missing { origin, valid } => {
                        let names = valid.join(", ");
                        let message = if origin == OriginKind::Unknown {
                            format!("{current_name} has no field {name:?} (valid: {names})")
                        } else {
                            format!(
                                "{current_name} has kind {origin}; field {name:?} does not exist (valid: {names})"
                            )
                        };
                        self.issue(span, message);
                        return Shape::unknown();
                    }
                },
            }
        }
        shape
    }
}

enum Segment<'a> {
    Field(&'a str),
    Index,
}

fn path_segments(path: &str) -> Vec<Segment<'_>> {
    let mut segments = Vec::new();
    let mut pos = 0;
    let bytes = path.as_bytes();
    while pos < bytes.len() {
        if bytes[pos] == b'.' {
            pos += 1;
            continue;
        }
        if bytes[pos] == b'[' {
            segments.push(Segment::Index);
            pos += 1;
            while pos < bytes.len() && bytes[pos] != b']' {
                pos += 1;
            }
            pos += usize::from(pos < bytes.len());
            continue;
        }
        let start = pos;
        while pos < bytes.len() && bytes[pos] != b'.' && bytes[pos] != b'[' {
            pos += 1;
        }
        segments.push(Segment::Field(&path[start..pos]));
    }
    segments
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use bpfman_shell_syntax::parse;

    use crate::{analyze_shapes, expand_imports, prepare_imports};

    fn messages(source: &str) -> Vec<String> {
        let parsed = parse("shape.bpfman", source).unwrap_or_else(|_| unreachable!());
        let plan = prepare_imports(parsed, Path::new(".")).unwrap_or_else(|_| unreachable!());
        let expanded = expand_imports(plan, &BTreeMap::new()).unwrap_or_else(|_| unreachable!());
        analyze_shapes(&expanded)
            .into_iter()
            .map(|issue| issue.message)
            .collect()
    }

    #[test]
    fn sealed_literals_records_and_lists_reject_proved_typos() {
        let found = messages(
            "let n = 42\nprint $n.value\nlet flag = true\nprint $flag.value\nlet r = record { a: [record { id: 1 }] }\nprint $r.a[0].missing",
        );
        assert_eq!(found.len(), 3);
        assert!(found[0].contains("n has kind scalar"));
        assert!(found[1].contains("flag has kind boolean"));
        assert!(found[2].contains("valid: id"));
    }

    #[test]
    fn bind_result_and_job_shapes_follow_go_contract() {
        let found = messages(
            "guard job <- start sleep 1\nprint $job.pid\nprint $job.exit_code\nlet r <- echo hi\nprint $r.code\nprint $r.exit_code",
        );
        assert_eq!(found.len(), 2);
        assert!(found[0].contains("job has kind job"));
        assert!(found[1].contains("r has kind result"));
        assert!(found[1].contains("exit_code"));
    }

    #[test]
    fn copies_inherit_leaf_shapes_and_frames_restore_outer_bindings() {
        let found = messages(
            "let r = record { count: 1 }\nlet n = $r.count\nif true { let n = record { id: 1 }\n print $n.id }\nprint $n.id",
        );
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("n has kind scalar"));
    }

    #[test]
    fn open_shapes_do_not_guess_domain_json_fields() {
        assert!(messages("let data = jq . $input\nprint $data.anything\nguard p <- bpfman program get 1\nprint $p.record.unverified").is_empty());
    }

    #[test]
    fn collect_shapes_preserve_indexed_producer_fields() {
        let found = messages(
            "guard jobs <- foreach x in [1] { start sleep $x }\nprint $jobs[0].pid\nprint $jobs[0].exit_code\nlet results <- foreach x in [1] { start sleep $x }\nprint $results.values[0].pid\nprint $results.results[0].value.pid\nprint $results.results[0].value.exit_code",
        );
        assert_eq!(found.len(), 2);
        assert!(found[0].contains("kind job"));
        assert!(found[1].contains("kind job"));
    }
}
