use bpfman_shell_ir::{MatchBlock, MatchPattern, MatchPredicate};

use super::{EvalResult, Machine, Value, is_empty};

impl Machine<'_> {
    pub(super) fn matches(&self, value: Value, block: &MatchBlock) -> EvalResult<bool> {
        if value.record_fields().is_none() && !matches!(value, Value::List(_) | Value::Outcome(_)) {
            return Err("matches requires a structured value".into());
        }
        self.matches_block(&value, block)
    }

    fn matches_block(&self, value: &Value, block: &MatchBlock) -> EvalResult<bool> {
        let mut matched = true;
        if block.exhaustive {
            let claimed = |name: &str| block.entries.iter().any(|entry| entry.path == name);
            matched = match value {
                Value::Outcome(outcome) => {
                    ["ok", "exit_code", "stdout", "stderr", "killed", "signal"]
                        .into_iter()
                        .all(claimed)
                        && (outcome.pid.is_none() || claimed("pid"))
                        && (outcome.primary == Value::Absent || claimed("value"))
                }
                value => value
                    .record_fields()
                    .is_some_and(|fields| fields.keys().all(|key| claimed(key))),
            };
        }

        for entry in &block.entries {
            let expected = match &entry.pattern {
                MatchPattern::Value(expression) => Some(self.eval(expression)?),
                _ => None,
            };
            let Ok(actual) = self.lookup_path(value.clone(), &entry.path) else {
                matched = false;
                continue;
            };
            if actual == Value::Absent {
                matched = false;
                continue;
            }
            let agrees = match &entry.pattern {
                MatchPattern::Value(_) => {
                    expected.is_some_and(|expected| equal(&actual, &expected))
                }
                MatchPattern::Nested(nested) => self.matches_block(&actual, nested)?,
                MatchPattern::Predicate(MatchPredicate::NotEmpty) => !is_empty(&actual),
                MatchPattern::Predicate(MatchPredicate::Null) => actual == Value::Null,
                MatchPattern::Predicate(MatchPredicate::Empty) => {
                    actual != Value::Null && is_empty(&actual)
                }
            };
            matched &= agrees;
        }
        Ok(matched)
    }
}

fn equal(actual: &Value, expected: &Value) -> bool {
    if matches!(actual, Value::Null | Value::Absent)
        || matches!(expected, Value::Null | Value::Absent)
    {
        return matches!(actual, Value::Null | Value::Absent)
            && matches!(expected, Value::Null | Value::Absent);
    }
    match (actual.scalar_text(), expected.scalar_text()) {
        (Some(actual), Some(expected)) => actual == expected,
        _ => actual == expected,
    }
}
