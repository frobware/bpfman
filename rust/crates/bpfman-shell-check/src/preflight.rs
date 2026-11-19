use bpfman_shell_source::{SourceName, Span};
use bpfman_shell_syntax::Statement;

use crate::{
    ExpandedProgram, Issue, analyze_calls, analyze_expanded_scopes, analyze_jobs, analyze_shapes,
};

/// An expanded program that passed all static analyses.
///
/// ```compile_fail
/// use bpfman_shell_check::{CheckedProgram, ExpandedProgram};
///
/// fn bypass_preflight(program: ExpandedProgram) -> CheckedProgram {
///     CheckedProgram(program)
/// }
/// ```
#[derive(Debug)]
pub struct CheckedProgram(ExpandedProgram);

impl CheckedProgram {
    /// Identity of the main source unit.
    #[must_use]
    pub fn source(&self) -> &SourceName {
        self.0.source()
    }

    /// Extent of the main source unit.
    #[must_use]
    pub fn span(&self) -> &Span {
        self.0.span()
    }

    /// Statements with their original source spans.
    #[must_use]
    pub fn statements(&self) -> &[Statement] {
        self.0.statements()
    }
}

/// Run all static analyses before allowing the normal lowering path.
///
/// # Errors
///
/// Returns the diagnostics if any static analysis rejects the program.
pub fn check(program: ExpandedProgram) -> Result<CheckedProgram, Vec<Issue>> {
    let mut issues = analyze_expanded_scopes(&program);
    issues.extend(analyze_calls(&program));
    issues.extend(analyze_shapes(&program));
    issues.extend(analyze_jobs(&program));
    if issues.is_empty() {
        Ok(CheckedProgram(program))
    } else {
        Err(issues)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::Path};

    use bpfman_shell_syntax::parse;

    use crate::{ExpandedProgram, check, expand_imports, prepare_imports};

    fn expanded(source: &str) -> ExpandedProgram {
        let parsed = parse("check.bpfman", source).unwrap_or_else(|error| unreachable!("{error}"));
        let plan =
            prepare_imports(parsed, Path::new(".")).unwrap_or_else(|error| unreachable!("{error}"));
        expand_imports(plan, &BTreeMap::new()).unwrap_or_else(|error| unreachable!("{error}"))
    }

    #[test]
    fn accepts_only_after_all_passes_and_retains_the_program() {
        let program = expanded("guard job <- start sleep 1\nwait $job");
        let checked = check(program.clone()).unwrap_or_else(|issues| unreachable!("{issues:?}"));
        assert_eq!(checked.source(), program.source());
        assert_eq!(checked.span(), program.span());
        assert_eq!(checked.statements(), program.statements());

        let program = expanded(
            "print $missing\ndef helper() { return 1 }\nlet x = helper\nlet y <- helpr\nlet b = true - 1\nguard job <- start sleep 1",
        );
        let found = check(program)
            .err()
            .unwrap_or_else(|| unreachable!("expected diagnostics"));
        for message in [
            "undefined variable",
            "did you mean `let x <- helper`",
            "unknown bind head \"helpr\"; did you mean \"helper\"?",
            "is not numeric",
            "has no matching wait or kill",
        ] {
            assert!(
                found.iter().any(|issue| issue.message.contains(message)),
                "{message}: {found:?}"
            );
        }
    }

    #[test]
    fn suggestions_are_deterministic_and_limited_to_unquoted_bind_heads() {
        let program = expanded(
            "def cat() { return 1 }\ndef car() { return 2 }\ndef star() { return 3 }\ndef atom() { return 4 }\nlet head = \"cap\"\nlet x <- $head\nlet y <- \"cap\"\nlet z <- ip addr\nlet job <- start sleep 1\ncap\nlet r <- cap",
        );
        let found = check(program)
            .err()
            .unwrap_or_else(|| unreachable!("expected diagnostics"));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            found[0].message,
            "unknown bind head \"cap\"; did you mean \"car\"?"
        );
        assert_eq!(found[0].span.start().line(), 11);
    }
}
