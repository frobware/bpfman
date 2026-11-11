//! Static call-site checks for hoisted user definitions and pure bind heads.

use std::collections::HashMap;

use bpfman_shell_semantics::{CommandArity, command_arity, pure_literal_issue};
use bpfman_shell_source::Span;
use bpfman_shell_syntax::{
    AssertionClause, Command, Expression, ExpressionKind, InterpolatedPart, MatchPattern,
    MatchesBlock, PureBuiltin, Statement, StatementKind, ThreadCall,
};

use crate::{ExpandedProgram, Issue};

struct DefInfo {
    arity: usize,
    span: Span,
}

struct Analysis {
    definitions: HashMap<String, DefInfo>,
    issues: Vec<Issue>,
}

/// Check known definition call arities, including threaded calls, and reject
/// pure builtins used as command-result bind heads. Dynamic heads remain open.
///
/// This is an incremental pure pass. An empty result does not certify execution.
#[must_use]
pub fn analyze_calls(program: &ExpandedProgram) -> Vec<Issue> {
    let mut definitions = HashMap::new();
    for statement in program.statements() {
        if let StatementKind::Definition(definition) = statement.kind() {
            definitions
                .entry(definition.name().text().to_owned())
                .or_insert_with(|| DefInfo {
                    arity: definition.parameters().len(),
                    span: definition.name().span().clone(),
                });
        }
    }
    let mut analysis = Analysis {
        definitions,
        issues: Vec::new(),
    };
    analysis.statements(program.statements());
    analysis.issues
}

impl Analysis {
    fn issue(&mut self, span: &Span, message: impl Into<String>) {
        self.issues.push(Issue {
            span: span.clone(),
            message: message.into(),
        });
    }

    fn statements(&mut self, statements: &[Statement]) {
        for statement in statements {
            self.statement(statement);
        }
    }

    fn statement(&mut self, statement: &Statement) {
        match statement.kind() {
            StatementKind::Command(command) | StatementKind::Defer(command) => {
                self.command(command, None)
            }
            StatementKind::Bind { name, command, .. } => self.command(command, Some(name.text())),
            StatementKind::Let { value, .. }
            | StatementKind::LetDestructure { value, .. }
            | StatementKind::Expression(value)
            | StatementKind::Return(value) => self.expression(value),
            StatementKind::If {
                first,
                elif,
                else_body,
            } => {
                self.expression(first.condition());
                self.statements(first.body());
                for branch in elif {
                    self.expression(branch.condition());
                    self.statements(branch.body());
                }
                if let Some(body) = else_body {
                    self.statements(body);
                }
            }
            StatementKind::Foreach { list, body, .. }
            | StatementKind::BindCollect { list, body, .. } => {
                self.expression(list);
                self.statements(body);
            }
            StatementKind::Poll { body, .. } => self.statements(body),
            StatementKind::Definition(definition) => self.statements(definition.body()),
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

    fn command(&mut self, command: &Command, bind_target: Option<&str>) {
        if let ExpressionKind::Word(name) = command.head().kind() {
            self.def_arity(name, command.arguments().len(), command.span());
            if !self.definitions.contains_key(name) {
                self.builtin_arity(name, command);
            }
            if let Some(target) = bind_target {
                if !self.definitions.contains_key(name) && PureBuiltin::from_name(name).is_some() {
                    let target = if target == "_" { "x" } else { target };
                    self.issue(command.span(), format!("{name} is a pure builtin; use 'let {target} = {name} ...' rather than '<-' (no result envelope is produced)"));
                }
            }
        }
        self.expression(command.head());
        for argument in command.arguments() {
            self.expression(argument);
        }
    }

    fn builtin_arity(&mut self, name: &str, command: &Command) {
        let Some(arity) = command_arity(name) else {
            return;
        };
        let got = command
            .arguments()
            .iter()
            .filter(|argument| {
                !(name == "kill"
                    && matches!(argument.kind(), ExpressionKind::Word(text) | ExpressionKind::Quoted(text) if text.starts_with("--signal=") || text.starts_with("--grace=")))
            })
            .count();
        let message = match arity {
            CommandArity::AtLeastOne | CommandArity::One if got == 0 => Some(format!(
                "{name}: expected at least 1 argument(s), got {got}"
            )),
            CommandArity::Zero if got > 0 => {
                Some(format!("{name}: expected at most 0 argument(s), got {got}"))
            }
            CommandArity::One if got > 1 => {
                Some(format!("{name}: expected at most 1 argument(s), got {got}"))
            }
            _ => None,
        };
        if let Some(message) = message {
            self.issue(command.span(), message);
        }
    }

    fn def_arity(&mut self, name: &str, got: usize, span: &Span) {
        let Some(definition) = self.definitions.get(name) else {
            return;
        };
        if got != definition.arity {
            self.issue(
                span,
                format!(
                    "{name}: expected {} argument(s), got {got} (def declared at {}:{})",
                    definition.arity,
                    definition.span.start().line(),
                    definition.span.start().column()
                ),
            );
        }
    }

    fn thread(&mut self, command: &ThreadCall) {
        if let ExpressionKind::Word(name) = command.head().kind() {
            self.def_arity(name, command.arguments().len() + 1, command.span());
        }
        self.expression(command.head());
        for argument in command.arguments() {
            self.expression(argument);
        }
    }

    fn expression(&mut self, value: &Expression) {
        match value.kind() {
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
                self.thread(command);
            }
            ExpressionKind::PureCall { builtin, arguments } => {
                if let [argument] = arguments.as_slice() {
                    if let Some(message) = pure_literal_issue(*builtin, argument) {
                        self.issue(argument.span(), message);
                    }
                }
                for argument in arguments {
                    self.expression(argument);
                }
            }
            ExpressionKind::List(arguments) => {
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
            ExpressionKind::Word(_)
            | ExpressionKind::Quoted(_)
            | ExpressionKind::Variable(_)
            | ExpressionKind::Adapter { .. } => {}
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
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use bpfman_shell_syntax::parse;

    use crate::{analyze_calls, expand_imports, prepare_imports};

    fn messages(source: &str) -> Vec<String> {
        let parsed = parse("calls.bpfman", source).unwrap_or_else(|_| unreachable!());
        let plan = prepare_imports(parsed, Path::new(".")).unwrap_or_else(|_| unreachable!());
        let expanded = expand_imports(plan, &BTreeMap::new()).unwrap_or_else(|_| unreachable!());
        analyze_calls(&expanded)
            .into_iter()
            .map(|issue| issue.message)
            .collect()
    }

    #[test]
    fn hoisted_definition_arity_applies_to_commands_binds_and_defer() {
        let found = messages("f 1\nlet x <- f 1 2 3\ndefer f\ndef f(a b) { return $a }");
        assert_eq!(found.len(), 3);
        assert!(found[0].contains("expected 2 argument(s), got 1"));
        assert!(found[1].contains("expected 2 argument(s), got 3"));
        assert!(found[2].contains("expected 2 argument(s), got 0"));
        assert!(messages("f 1 2\ndef f(a b) { return $a }").is_empty());
    }

    #[test]
    fn threaded_value_counts_as_one_definition_argument() {
        let found = messages(
            "def pair(a b) { return $a }\nlet x = 1 |> pair\nlet y = 1 |> pair 2\nlet z = 1 |> pair 2 3",
        );
        assert_eq!(found.len(), 2);
        assert!(found[0].contains("got 1"));
        assert!(found[1].contains("got 3"));
    }

    #[test]
    fn pure_bind_is_rejected_unless_a_def_shadows_the_name() {
        let found = messages("let x <- jq .id $v\nguard _ <- range 3");
        assert_eq!(found.len(), 2);
        assert!(found[0].contains("jq is a pure builtin"));
        assert!(found[1].contains("let x = range"));
        assert!(messages("def jq(x) { return $x }\nlet y <- jq 3").is_empty());
    }

    #[test]
    fn dynamic_command_heads_stay_open() {
        assert!(messages("let x <- ($command) 3\ndefer ($command) 1").is_empty());
    }

    #[test]
    fn pure_builtin_literals_follow_the_go_checker_contract() {
        let found = messages(
            "let a = u32le (-1)\nlet b = u32le 4294967296\nlet c = u64le (-1)\nlet d = range (-1)\nlet e = range 2147483648",
        );
        assert_eq!(found.len(), 5);
        assert!(found[0].contains("u32le: negative values are not representable"));
        assert_eq!(found[1], "u32le: value 4294967296 does not fit in 32 bits");
        assert!(found[2].contains("u64le: negative values are not representable"));
        assert!(found[3].contains("range: negative bound is not allowed"));
        assert_eq!(
            found[4],
            "range: bound 2147483648 exceeds the maximum of 2147483647"
        );
    }

    #[test]
    fn pure_builtin_literal_checks_preserve_open_and_valid_inputs() {
        assert!(messages(
            "let a = u32le '0xffffffff'\nlet b = u64le '18446744073709551615'\nlet c = range '0b101'\nlet d = range $n\nlet e = u32le (1 + 2)\nlet f = range '0_7'"
        ).is_empty());
        let found = messages(
            "let a = u32le '0xGG'\nlet b = range '08'\nlet c = u64le '18446744073709551616'\nlet d = range '+1'",
        );
        assert_eq!(found.len(), 4);
        assert!(
            found
                .iter()
                .all(|message| message.contains("invalid integer"))
        );
    }

    #[test]
    fn pure_literal_issues_point_to_the_argument_even_when_nested() {
        let parsed = parse("nested.bpfman", "let x = [range 2147483648 u32le '']")
            .unwrap_or_else(|_| unreachable!());
        let plan = prepare_imports(parsed, Path::new(".")).unwrap_or_else(|_| unreachable!());
        let expanded = expand_imports(plan, &BTreeMap::new()).unwrap_or_else(|_| unreachable!());
        let issues = analyze_calls(&expanded);
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].span.start().column(), 16);
        assert_eq!(
            issues[0].message,
            "range: bound 2147483648 exceeds the maximum of 2147483647"
        );
        assert_eq!(issues[1].message, "u32le: empty argument");
    }

    #[test]
    fn native_job_command_arity_matches_go_positional_rules() {
        let found = messages(
            "start\nfire\nwait\nkill --signal=USR1\njobs extra\nreap extra\nwait one two\nkill --grace=2s one two",
        );
        assert_eq!(found.len(), 8);
        assert_eq!(found[0], "start: expected at least 1 argument(s), got 0");
        assert_eq!(found[1], "fire: expected at least 1 argument(s), got 0");
        assert_eq!(found[2], "wait: expected at least 1 argument(s), got 0");
        assert_eq!(found[3], "kill: expected at least 1 argument(s), got 0");
        assert_eq!(found[4], "jobs: expected at most 0 argument(s), got 1");
        assert_eq!(found[5], "reap: expected at most 0 argument(s), got 1");
        assert_eq!(found[6], "wait: expected at most 1 argument(s), got 2");
        assert_eq!(found[7], "kill: expected at most 1 argument(s), got 2");
    }

    #[test]
    fn native_command_checks_respect_def_precedence_and_dynamic_heads() {
        assert!(messages("def start() { print ok }\nstart").is_empty());
        assert!(messages("def kill(a) { print $a }\nkill --signal=BAD").is_empty());
        assert!(
            messages("let x <- ($command) one two\nstart sleep 1\nkill --signal=USR1 $job")
                .is_empty()
        );
        assert_eq!(
            messages("jobs '--flag'"),
            vec!["jobs: expected at most 0 argument(s), got 1".to_owned()]
        );
        assert_eq!(
            messages("kill --signa=USR1 $job"),
            vec!["kill: expected at most 1 argument(s), got 2".to_owned()]
        );
        assert!(messages("start --hello").is_empty());
    }

    #[test]
    fn imported_definitions_are_hoisted_into_call_analysis() {
        let parsed = parse("main.bpfman", "call").unwrap_or_else(|_| unreachable!());
        let plan = prepare_imports(parsed, Path::new(".")).unwrap_or_else(|_| unreachable!());
        let expanded = expand_imports(plan, &BTreeMap::new()).unwrap_or_else(|_| unreachable!());
        assert!(analyze_calls(&expanded).is_empty());

        let parsed =
            parse("main.bpfman", "import lib.bpfman\ncall").unwrap_or_else(|_| unreachable!());
        let plan = prepare_imports(parsed, Path::new(".")).unwrap_or_else(|_| unreachable!());
        let gathered = BTreeMap::from([(
            PathBuf::from("lib.bpfman"),
            "def call(x) { return $x }".to_owned(),
        )]);
        let expanded = expand_imports(plan, &gathered).unwrap_or_else(|_| unreachable!());
        let issues = analyze_calls(&expanded);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("expected 1 argument(s), got 0"));
        assert_eq!(issues[0].span.start().name().as_str(), "main.bpfman");
    }
}
