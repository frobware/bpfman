//! Name-based preflight for the Go shell's direct background-job idiom.

use std::collections::HashSet;

use bpfman_shell_syntax::{
    BindMode, Command, Expression, ExpressionKind, Identifier, Statement, StatementKind,
    VariableRef,
};

use crate::{ExpandedProgram, Issue};

/// Report named `guard ... <- start` jobs with no recognizable cleanup in the
/// same script or definition body. Direct returns from a definition transfer
/// ownership to the caller. Hoisted definitions take precedence over builtins.
///
/// The check matches names across branches and leaves execution paths, alias
/// tracking and dynamic job dispatch to runtime.
#[must_use]
pub fn analyze_jobs(program: &ExpandedProgram) -> Vec<Issue> {
    let definitions = program
        .statements()
        .iter()
        .filter_map(|statement| match statement.kind() {
            StatementKind::Definition(definition) => Some(definition.name().text()),
            _ => None,
        })
        .collect();
    let mut issues = Vec::new();
    check_body(program.statements(), &definitions, false, &mut issues);
    issues
}

fn check_body(
    statements: &[Statement],
    definitions: &HashSet<&str>,
    in_definition: bool,
    issues: &mut Vec<Issue>,
) {
    let mut body = Body {
        definitions,
        in_definition,
        started: Vec::new(),
        managed: HashSet::new(),
    };
    body.walk(statements, issues);
    for name in body.started {
        if !body.managed.contains(name.text()) {
            issues.push(Issue {
                span: name.span().clone(),
                message: format!("started job {:?} has no matching wait or kill", name.text()),
            });
        }
    }
}

struct Body<'a, 'defs> {
    definitions: &'defs HashSet<&'defs str>,
    in_definition: bool,
    started: Vec<&'a Identifier>,
    managed: HashSet<&'a str>,
}

impl<'a> Body<'a, '_> {
    fn walk(&mut self, statements: &'a [Statement], issues: &mut Vec<Issue>) {
        for statement in statements {
            match statement.kind() {
                StatementKind::Bind {
                    mode,
                    name,
                    command,
                } => {
                    let head = self.native_head(command);
                    if *mode == BindMode::Guard && name.text() != "_" && head == Some("start") {
                        self.started.push(name);
                    }
                    self.manage(head, command);
                }
                StatementKind::Command(command) | StatementKind::Defer(command) => {
                    self.manage(self.native_head(command), command);
                }
                StatementKind::Return(value) if self.in_definition => {
                    if let ExpressionKind::Variable(variable) = value.kind() {
                        if variable.path.is_empty() {
                            self.managed.insert(variable.name.as_str());
                        }
                    }
                }
                StatementKind::If {
                    first,
                    elif,
                    else_body,
                } => {
                    self.walk(first.body(), issues);
                    for branch in elif {
                        self.walk(branch.body(), issues);
                    }
                    if let Some(body) = else_body {
                        self.walk(body, issues);
                    }
                }
                StatementKind::Foreach { body, .. } | StatementKind::Poll { body, .. } => {
                    self.walk(body, issues);
                }
                StatementKind::Definition(definition) => {
                    check_body(definition.body(), self.definitions, true, issues);
                }
                _ => {}
            }
        }
    }

    fn native_head(&self, command: &'a Command) -> Option<&'a str> {
        let name = match command.head().kind() {
            ExpressionKind::Word(name) | ExpressionKind::Quoted(name) => name.as_str(),
            _ => return None,
        };
        (!self.definitions.contains(name)).then_some(name)
    }

    fn manage(&mut self, head: Option<&str>, command: &'a Command) {
        if matches!(head, Some("wait" | "kill")) {
            if let Some((_, variable)) = job_reference(command) {
                self.managed.insert(variable.name.as_str());
            }
        }
    }
}

pub(super) fn job_reference(command: &Command) -> Option<(&Expression, &VariableRef)> {
    // Go matches the first direct variable argument.
    command
        .arguments()
        .iter()
        .find_map(|argument| match argument.kind() {
            ExpressionKind::Variable(variable) => Some((argument, variable)),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::Path};

    use bpfman_shell_syntax::parse;

    use crate::{Issue, analyze_jobs, expand_imports, prepare_imports};

    fn issues(source: &str) -> Vec<Issue> {
        let parsed = parse("jobs.bpfman", source).unwrap_or_else(|error| unreachable!("{error}"));
        let plan = prepare_imports(parsed, Path::new(".")).unwrap_or_else(|_| unreachable!());
        let program = expand_imports(plan, &BTreeMap::new()).unwrap_or_else(|_| unreachable!());
        analyze_jobs(&program)
    }

    #[test]
    fn reports_named_guard_starts_at_the_binding() {
        let found = issues(
            "guard job <- start sleep 60\nguard _ <- start sleep 60\nlet result <- start sleep 60",
        );
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].message,
            "started job \"job\" has no matching wait or kill"
        );
        assert_eq!(found[0].span.start().column(), 7);
    }

    #[test]
    fn recognizes_direct_bind_and_deferred_cleanup() {
        for cleanup in [
            "wait $job",
            "let status <- wait $job",
            "kill $job",
            "defer kill --signal=USR1 $job",
        ] {
            assert!(issues(&format!("guard job <- start sleep 60\n{cleanup}")).is_empty());
        }
    }

    #[test]
    fn definitions_have_separate_job_sets_and_can_return_ownership() {
        assert!(issues("def spawn() { guard job <- start sleep 60\n return $job }\nguard job <- spawn\nwait $job").is_empty());
        let found = issues(
            "def spawn() { if true { guard job <- start sleep 60 }\n return 7 }\nguard job <- spawn\nwait $job",
        );
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].message,
            "started job \"job\" has no matching wait or kill"
        );
    }

    #[test]
    fn hoisted_definitions_shadow_job_builtins() {
        assert!(issues("guard job <- start 1\ndef start(arg) { return $arg }").is_empty());
        for head in ["kill", "wait"] {
            let found = issues(&format!(
                "guard job <- start sleep 60\n{head} $job\ndef {head}(arg) {{ print $arg }}"
            ));
            assert_eq!(found.len(), 1);
        }
    }
}
