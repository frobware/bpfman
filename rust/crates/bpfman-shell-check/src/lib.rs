//! Pure scope and control-flow analysis for parsed bpfman-shell programs.
//!
//! `analyze_scopes` is an incremental checker pass. It reports only the
//! contracts documented here and does not produce a checked executable program.

use std::collections::{HashMap, HashSet};
use std::fmt;

use bpfman_shell_source::Span;
use bpfman_shell_syntax::{
    AssertionClause, AssertionMode, Command, Expression, ExpressionKind, InterpolatedPart,
    MatchPattern, MatchesBlock, ParsedProgram, Statement, StatementKind,
};

mod calls;
mod imports;
mod shapes;

pub use calls::analyze_calls;
pub use imports::{
    ExpandedProgram, ImportError, ImportPlan, ImportRequest, expand_imports, prepare_imports,
};
pub use shapes::analyze_shapes;

/// One static finding at its original source extent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Issue {
    /// Source extent of the offending form.
    pub span: Span,
    /// Diagnostic without location formatting.
    pub message: String,
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}: {}",
            self.span.start().line(),
            self.span.start().column(),
            self.message
        )
    }
}

/// Report undefined variables, duplicate and misplaced definitions, invalid
/// control-flow placement, and imports below module top level.
///
/// This is a pure partial pass: an empty result does not authorize execution.
#[must_use]
pub fn analyze_scopes(program: &ParsedProgram) -> Vec<Issue> {
    analyze_statements(program.statements())
}

/// Run the same partial scope analysis on a program after direct-import
/// expansion. Imported definitions retain their library source locations.
#[must_use]
pub fn analyze_expanded_scopes(program: &ExpandedProgram) -> Vec<Issue> {
    analyze_statements(program.statements())
}

fn analyze_statements(statements: &[Statement]) -> Vec<Issue> {
    let mut analysis = Analysis::new();
    analysis.prescan_defs(statements);
    analysis.statements(statements);
    analysis.issues
}

struct Analysis {
    frames: Vec<HashSet<String>>,
    defs: HashMap<String, Span>,
    issues: Vec<Issue>,
    def_depth: usize,
    loop_depth: usize,
    poll_depth: usize,
    block_depth: usize,
}

impl Analysis {
    fn new() -> Self {
        Self {
            frames: vec![HashSet::new()],
            defs: HashMap::new(),
            issues: Vec::new(),
            def_depth: 0,
            loop_depth: 0,
            poll_depth: 0,
            block_depth: 0,
        }
    }

    fn issue(&mut self, span: &Span, message: impl Into<String>) {
        self.issues.push(Issue {
            span: span.clone(),
            message: message.into(),
        });
    }

    fn prescan_defs(&mut self, statements: &[Statement]) {
        for statement in statements {
            let StatementKind::Definition(definition) = statement.kind() else {
                continue;
            };
            let name = definition.name();
            if let Some(previous) = self.defs.get(name.text()) {
                self.issue(
                    name.span(),
                    format!(
                        "duplicate top-level def {:?}; previous declaration at {}:{}:{}",
                        name.text(),
                        previous.start().name().as_str(),
                        previous.start().line(),
                        previous.start().column()
                    ),
                );
            } else {
                self.defs
                    .insert(name.text().to_owned(), name.span().clone());
            }
        }
    }

    fn define(&mut self, name: &str) {
        if name != "_" {
            if let Some(frame) = self.frames.last_mut() {
                frame.insert(name.to_owned());
            }
        }
    }

    fn known(&self, name: &str) -> bool {
        self.frames.iter().rev().any(|frame| frame.contains(name))
    }

    fn scoped(&mut self, bindings: impl IntoIterator<Item = String>, body: &[Statement]) {
        self.frames.push(HashSet::new());
        self.block_depth += 1;
        for name in bindings {
            self.define(&name);
        }
        self.statements(body);
        self.block_depth -= 1;
        self.frames.pop();
    }

    fn statements(&mut self, statements: &[Statement]) {
        for statement in statements {
            self.statement(statement);
        }
    }

    fn statement(&mut self, statement: &Statement) {
        match statement.kind() {
            StatementKind::Command(command) => {
                self.command(command);
                if self.block_depth > 0 && is_import(command) {
                    self.issue(statement.span(), "import must be declared at top level");
                }
            }
            StatementKind::Expression(value) | StatementKind::Return(value) => {
                if matches!(statement.kind(), StatementKind::Return(_)) && self.def_depth == 0 {
                    self.issue(statement.span(), "return outside a def body");
                }
                self.expression(value);
            }
            StatementKind::Let { name, value } => {
                self.expression(value);
                self.define(name.text());
            }
            StatementKind::LetDestructure { names, value } => {
                self.expression(value);
                for name in names {
                    self.define(name.text());
                }
            }
            StatementKind::Bind { name, command, .. } => {
                self.command(command);
                self.define(name.text());
            }
            StatementKind::BindCollect {
                name,
                bindings,
                list,
                body,
                ..
            } => {
                self.expression(list);
                self.loop_depth += 1;
                self.scoped(
                    bindings.names().iter().map(|name| name.text().to_owned()),
                    body,
                );
                self.loop_depth -= 1;
                self.define(name.text());
            }
            StatementKind::Defer(command) => self.command(command),
            StatementKind::Definition(definition) => {
                if self.block_depth > 0 {
                    self.issue(
                        definition.name().span(),
                        format!(
                            "def {:?} must be declared at top level",
                            definition.name().text()
                        ),
                    );
                }
                self.def_depth += 1;
                let saved_loop = self.loop_depth;
                self.loop_depth = 0;
                self.scoped(
                    definition
                        .parameters()
                        .iter()
                        .map(|param| param.name().text().to_owned()),
                    definition.body(),
                );
                self.loop_depth = saved_loop;
                self.def_depth -= 1;
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
                self.loop_depth += 1;
                self.scoped(
                    bindings.names().iter().map(|name| name.text().to_owned()),
                    body,
                );
                self.loop_depth -= 1;
            }
            StatementKind::Poll { body, .. } => {
                self.poll_depth += 1;
                self.scoped([], body);
                self.poll_depth -= 1;
            }
            StatementKind::Retry { message, unless } => {
                if self.poll_depth == 0 && self.def_depth == 0 {
                    self.issue(statement.span(), "retry is only valid inside poll");
                }
                if let Some(value) = message {
                    self.expression(value);
                }
                if let Some(value) = unless {
                    self.expression(value);
                }
            }
            StatementKind::Assertion { mode, clause } => {
                if self.poll_depth > 0 && *mode == AssertionMode::Assert {
                    self.issue(
                        statement.span(),
                        "assert is not valid inside poll; use retry unless ... or require ...",
                    );
                }
                match clause {
                    AssertionClause::Expression(value) => self.expression(value),
                    AssertionClause::CommandStatus { arguments, .. } => {
                        for value in arguments {
                            self.expression(value);
                        }
                    }
                }
            }
            StatementKind::Break | StatementKind::Continue => {
                if self.loop_depth == 0 {
                    let word = if matches!(statement.kind(), StatementKind::Break) {
                        "break"
                    } else {
                        "continue"
                    };
                    self.issue(
                        statement.span(),
                        format!("'{word}' outside any foreach loop"),
                    );
                }
            }
        }
    }

    fn command(&mut self, command: &Command) {
        self.expression(command.head());
        for value in command.arguments() {
            self.expression(value);
        }
    }

    fn expression(&mut self, value: &Expression) {
        match value.kind() {
            ExpressionKind::Variable(variable) | ExpressionKind::Adapter { variable, .. } => {
                if !self.known(&variable.name) {
                    self.issue(
                        value.span(),
                        format!("undefined variable: {}", variable.name),
                    );
                }
            }
            ExpressionKind::Interpolated(parts) => {
                for part in parts {
                    if let InterpolatedPart::Expression(value) = part {
                        self.expression(value);
                    }
                }
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
}

fn is_import(command: &Command) -> bool {
    matches!(command.head().kind(), ExpressionKind::Word(word) if word == "import")
}

#[cfg(test)]
mod tests {
    use bpfman_shell_syntax::parse;

    use super::analyze_scopes;

    fn messages(source: &str) -> Vec<String> {
        let program = parse("scope.bpfman", source).unwrap_or_else(|_| unreachable!());
        analyze_scopes(&program)
            .into_iter()
            .map(|issue| issue.message)
            .collect()
    }

    #[test]
    fn bindings_become_visible_after_their_rhs_and_leave_with_their_frame() {
        let found =
            messages("let x = $x\nlet y = 1\nif true { let z = $y } else { print $z }\nprint $z");
        assert_eq!(
            found,
            [
                "undefined variable: x",
                "undefined variable: z",
                "undefined variable: z"
            ]
        );
        assert!(messages("let (a b) = [1 2]\nprint $a $b").is_empty());
        assert_eq!(
            messages("let xs <- foreach x in $missing { echo $x }\nprint $x"),
            ["undefined variable: missing", "undefined variable: x"]
        );
    }

    #[test]
    fn expression_walk_reaches_nested_matches_records_and_interpolations() {
        let found = messages(
            "let r = record { x: $a }\nassert $r matches { x: matches { id: $b } }\nprint \"${c}\"",
        );
        assert_eq!(
            found,
            [
                "undefined variable: a",
                "undefined variable: b",
                "undefined variable: c"
            ]
        );
    }

    #[test]
    fn placement_checks_follow_go_frames() {
        let found = messages(
            "return $x\nbreak\ncontinue\nretry\npoll timeout 1s every 1ms { assert true\n require true\n retry }\nif true { def inner() { return 1 }\n import lib }",
        );
        assert!(
            found
                .iter()
                .any(|message| message == "return outside a def body")
        );
        assert!(
            found
                .iter()
                .any(|message| message == "'break' outside any foreach loop")
        );
        assert!(
            found
                .iter()
                .any(|message| message == "'continue' outside any foreach loop")
        );
        assert!(
            found
                .iter()
                .any(|message| message == "retry is only valid inside poll")
        );
        assert!(
            found
                .iter()
                .any(|message| message.starts_with("assert is not valid inside poll"))
        );
        assert!(
            found
                .iter()
                .any(|message| message.contains("must be declared at top level"))
        );
        assert!(
            found
                .iter()
                .any(|message| message == "import must be declared at top level")
        );
    }

    #[test]
    fn loop_exits_do_not_escape_into_nested_defs_and_duplicates_are_reported() {
        let found = messages(
            "def a() { return 1 }\ndef a() { return 2 }\nforeach x in [1] { break\n def b() { continue } }",
        );
        assert!(
            found
                .iter()
                .any(|message| message.contains("duplicate top-level def"))
        );
        assert!(
            found
                .iter()
                .any(|message| message == "'continue' outside any foreach loop")
        );
        assert!(
            !found
                .iter()
                .any(|message| message == "'break' outside any foreach loop")
        );
    }
}
