//! Conservative shape propagation and proved field-access diagnostics.

use std::collections::HashMap;

use bpfman_shell_semantics::{
    FieldLookup, OriginKind, Shape, collect_shape, kind_shape, outcome_shape, pure_shape,
};
use bpfman_shell_source::Span;
use bpfman_shell_syntax::{
    AssertionClause, BinaryOperator, BindMode, Command, Definition, Expression, ExpressionKind,
    InterpolatedPart, MatchPattern, MatchesBlock, Statement, StatementKind, VariableRef,
    is_json_number,
};

use crate::{ExpandedProgram, Issue};

mod arguments;
mod returns;

/// Report proved field-access mistakes, incompatible comparison operands, and
/// provably nonnumeric arithmetic operands.
/// Unknown and domain shapes without a verified Rust JSON projection stay open.
/// An empty result does not certify execution.
#[must_use]
pub fn analyze_shapes(program: &ExpandedProgram) -> Vec<Issue> {
    let defs = program
        .statements()
        .iter()
        .filter_map(|statement| match statement.kind() {
            StatementKind::Definition(definition) => Some((definition.name().text(), definition)),
            _ => None,
        })
        .collect();
    let mut analysis = Analysis {
        frames: vec![HashMap::new()],
        defs,
        returns: HashMap::new(),
        issues: Vec::new(),
    };
    analysis.statements(program.statements());
    analysis.issues
}

struct Analysis<'a> {
    frames: Vec<HashMap<String, Binding>>,
    defs: HashMap<&'a str, &'a Definition>,
    returns: HashMap<&'a str, returns::Inference>,
    issues: Vec<Issue>,
}

struct Binding {
    shape: Shape,
    nonnumeric_literal: Option<String>,
}

impl Analysis<'_> {
    fn issue(&mut self, span: &Span, message: impl Into<String>) {
        self.issues.push(Issue {
            span: span.clone(),
            message: message.into(),
        });
    }

    fn define(&mut self, name: &str, shape: Shape) {
        self.bind(name, shape, None);
    }

    fn bind(&mut self, name: &str, shape: Shape, nonnumeric_literal: Option<String>) {
        if name != "_" {
            if let Some(frame) = self.frames.last_mut() {
                frame.insert(
                    name.to_owned(),
                    Binding {
                        shape,
                        nonnumeric_literal,
                    },
                );
            }
        }
    }

    fn lookup(&self, name: &str) -> Option<&Binding> {
        self.frames.iter().rev().find_map(|frame| frame.get(name))
    }

    fn push_scope(&mut self, names: impl IntoIterator<Item = String>) {
        self.frames.push(HashMap::new());
        for name in names {
            self.define(&name, Shape::unknown());
        }
    }

    fn scoped(&mut self, names: impl IntoIterator<Item = String>, body: &[Statement]) {
        self.push_scope(names);
        self.statements(body);
        self.frames.pop();
    }

    fn bind_shape(&mut self, mode: BindMode, command: &Command) -> Shape {
        let primary = self.primary_shape(command);
        match mode {
            BindMode::Guard => primary,
            BindMode::Let => outcome_shape(primary),
        }
    }

    fn collect_primary_shape(&mut self, body: &[Statement]) -> Shape {
        match body.last().map(Statement::kind) {
            Some(StatementKind::Command(command)) => self.primary_shape(command),
            _ => Shape::unknown(),
        }
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
                let nonnumeric_literal = match value.kind() {
                    ExpressionKind::Quoted(text) => Some(text.clone()),
                    ExpressionKind::Word(text)
                        if shape.origin() == OriginKind::Scalar && !is_json_number(text) =>
                    {
                        Some(text.clone())
                    }
                    _ => None,
                };
                self.bind(name.text(), shape, nonnumeric_literal);
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
                let shape = self.bind_shape(*mode, command);
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
                let primary = self.collect_primary_shape(body);
                self.define(
                    name.text(),
                    collect_shape(primary, *mode == BindMode::Guard),
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
        self.check_native_argument(command);
    }

    fn expression(&mut self, value: &Expression) {
        match value.kind() {
            ExpressionKind::Variable(variable) | ExpressionKind::Adapter { variable, .. } => {
                self.path_shape(variable, value.span());
            }
            ExpressionKind::Binary {
                operator,
                left,
                right,
            } => {
                let mut arithmetic_issues = Vec::new();
                crate::arithmetic::check(
                    *operator,
                    [left, right],
                    |name| self.defs.contains_key(name),
                    |operand| self.arithmetic_variable_issue(operand),
                    &mut arithmetic_issues,
                );
                self.issues.extend(arithmetic_issues);
                if let Some(issue) = crate::comparison::issue(
                    value,
                    |operand| self.infer(operand),
                    |name| self.defs.contains_key(name),
                ) {
                    self.issues.push(issue);
                }
                self.expression(left);
                self.expression(right);
            }
            ExpressionKind::Logical { left, right, .. } => {
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

    fn arithmetic_variable_issue(&self, operand: &Expression) -> Option<String> {
        let ExpressionKind::Variable(variable) = operand.kind() else {
            return None;
        };
        let separator = if variable.path.is_empty() || variable.path.starts_with('[') {
            ""
        } else {
            "."
        };
        let name = format!("{}{separator}{}", variable.name, variable.path);
        let shape = self.path_shape_readonly(variable);
        match shape.origin() {
            OriginKind::Scalar | OriginKind::Unknown => {
                if variable.path.is_empty() {
                    if let Some(text) = &self.lookup(&variable.name)?.nonnumeric_literal {
                        return Some(format!("{name} is {text:?}, not a number"));
                    }
                }
                None
            }
            OriginKind::Bool => Some(format!("{name} is a boolean, not a number")),
            OriginKind::Null => Some(format!("{name} is null, not a number")),
            kind => Some(format!("{name} has kind {kind}, not a number")),
        }
    }

    fn path_shape_readonly(&self, variable: &VariableRef) -> Shape {
        let Some(mut shape) = self
            .lookup(&variable.name)
            .map(|binding| binding.shape.clone())
        else {
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
        let Some(mut shape) = self
            .lookup(&variable.name)
            .map(|binding| binding.shape.clone())
        else {
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
        let parsed = parse("shape.bpfman", source).unwrap_or_else(|error| unreachable!("{error}"));
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

    #[test]
    fn comparison_checks_known_shapes_and_leaves_open_values_to_runtime() {
        let setup = "let r = record { id: 1 }\nlet flag = true\nlet open = jq . 1";
        for expression in [
            "$r == null",
            "null == $r",
            "$r != null",
            "null != $r",
            "$open == $r",
            "$flag == false",
        ] {
            assert!(messages(&format!("{setup}\nlet result = {expression}")).is_empty());
        }
        for (expression, expected) in [
            ("$r < null", "null supports only == and !="),
            ("null >= $r", "null supports only == and !="),
            ("$r == $r", "only scalars"),
            ("$flag == 1", "cannot compare boolean to scalar"),
            ("$flag < true", "booleans support only == and !="),
        ] {
            let found = messages(&format!("{setup}\nlet result = {expression}"));
            assert_eq!(found.len(), 1, "{expression}: {found:?}");
            assert!(found[0].contains(expected), "{expression}: {found:?}");
        }
    }

    #[test]
    fn comparison_hints_at_hoisted_definition_calls() {
        let found = messages("let result = two == 2\ndef two() { return 2 }");
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("call it via `let v <- two`"));
        assert!(messages("let result = \"two\" == \"2\"\ndef two() { return 2 }").is_empty());
    }

    #[test]
    fn arithmetic_checks_each_literal_including_inside_interpolation() {
        let found = messages("let result = bad * bogus\ndef bad() { return 3 }");
        assert_eq!(found.len(), 2);
        assert!(found[0].contains("call it via `let v <- bad`"));
        assert_eq!(found[1], "arithmetic *: operand \"bogus\" is not numeric");
        assert_eq!(
            messages("print \"${4 * bogus}\""),
            ["arithmetic *: operand \"bogus\" is not numeric"]
        );
    }

    #[test]
    fn arithmetic_accepts_numeric_literals_and_leaves_dynamic_inputs_open() {
        assert!(messages("let result = 1.5 + 2e3\nlet n = $input * 3").is_empty());
        assert_eq!(messages("let result = \"NaN\" / 2").len(), 1);
    }

    #[test]
    fn arithmetic_uses_lexical_binding_evidence() {
        let found = messages(
            "let n = word\nif true { let n = 2\n let local = $n * 3 }\nlet outer = $n * 3\nlet quoted = \"4\"\nlet result = $quoted + 1\nlet data = record { flag: true }\nlet flag = $data.flag - 1\nlet nothing = null\nlet n = $nothing / 2\nlet open = jq . 1\nlet dynamic = $open * 2",
        );
        assert_eq!(
            found,
            [
                "arithmetic *: n is \"word\", not a number",
                "arithmetic +: quoted is \"4\", not a number",
                "arithmetic -: data.flag is a boolean, not a number",
                "arithmetic /: nothing is null, not a number",
            ]
        );
    }

    #[test]
    fn definition_return_shapes_flow_through_hoisted_bind_and_collect() {
        let found = messages(
            "guard direct <- outer\nprint $direct.id\nprint $direct.typo\nlet wrapped <- inner\nprint $wrapped.value.typo\nguard collected <- foreach n in [1] { inner }\nprint $collected[0].typo\ndef outer() { guard value <- inner\n return $value }\ndef inner() { let value = record { id: 1 }\n return $value }",
        );
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(found.iter().all(|issue| issue.contains("valid: id")));
    }

    #[test]
    fn exhaustive_returns_join_only_matching_shapes() {
        let found = messages(
            "def same(flag) { if $flag { return record { id: 1 } }\n return record { id: 2 } }\ndef different(flag) { if $flag { return record { id: 1 } } else { return record { name: x } } }\ndef partial(flag) { if $flag { return record { id: 1 } } }\nguard a <- same true\nprint $a.typo\nguard b <- different true\nprint $b.typo\nguard c <- partial true\nprint $c.typo",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("a has no field"));
    }

    #[test]
    fn recursive_definitions_and_caller_bindings_stay_open() {
        assert!(messages(
            "let caller = record { id: 1 }\ndef local() { return $caller }\ndef first() { guard value <- second\n return $value }\ndef second() { guard value <- first\n return $value }\nguard a <- first\nprint $a.typo\nguard b <- local\nprint $b.typo"
        ).is_empty());
    }

    #[test]
    fn no_return_definition_has_result_shape_but_loops_may_return() {
        let found = messages(
            "def no_return() { print ok }\ndef maybe() { foreach n in [1] { return record { id: 1 } } }\nguard a <- no_return\nprint $a.exit_code\nprint $a.typo\nguard b <- maybe\nprint $b.typo",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("a has kind result"));
    }

    #[test]
    fn job_commands_check_the_selected_value_shape() {
        let found = messages(
            "let scalar = 1\nwait $scalar\ndefer kill --signal=TERM $scalar\nlet started <- start sleep 1\nwait $started.value\nwait $started\nlet open = jq . 1\nwait $open",
        );
        assert_eq!(
            found,
            [
                "expected a $job argument, got a scalar value",
                "expected a $job argument, got a scalar value",
                "expected a $job argument, got a result value",
            ]
        );
        assert!(
            messages("let scalar = 1\nwait $scalar\ndef wait(value) { print $value }").is_empty()
        );
    }

    #[test]
    fn network_commands_distinguish_pairs_and_endpoints() {
        let found = messages(
            "guard pair <- net netns-veth-pair\nnet exec $pair.a true\nnet start $pair.b true\nnet release $pair\nnet exec $pair true\nnet release $pair.a\nlet scalar = 1\nnet start $scalar true\nlet open = jq . 1\nnet release $open",
        );
        assert_eq!(
            found,
            [
                "net exec: netns-veth-pair has two endpoints; use $pair.a or $pair.b",
                "net release: endpoint belongs to a netns-veth-pair; release the pair",
                "net start: expected a $pair or endpoint argument, got a scalar value",
            ]
        );
        assert!(
            messages("let scalar = 1\nnet release $scalar\ndef net(action value) { print $value }")
                .is_empty()
        );
    }
}
