use bpfman_shell_ir::{MatchBlock, MatchPattern, MatchPredicate};
use bpfman_shell_source::Span;

use super::{EvalResult, Machine, Value, is_empty, location};

#[derive(Default)]
pub(super) struct MatchReport(Vec<(Span, String)>);

impl MatchReport {
    pub(super) fn passed(&self) -> bool {
        self.0.is_empty()
    }

    pub(super) fn describe(&self) -> String {
        let count = self.0.len();
        let noun = if count == 1 { "mismatch" } else { "mismatches" };
        let mut message = format!("matches: {count} {noun}");
        for (span, row) in &self.0 {
            message.push_str(&format!("\n  {}: {row}", location(span)));
        }
        message
    }

    fn record(&mut self, span: &Span, message: String) {
        self.0.push((span.clone(), message));
    }
}

impl Machine<'_> {
    pub(super) fn matches(&self, value: Value, block: &MatchBlock) -> EvalResult<MatchReport> {
        if value.record_fields().is_none() && !matches!(value, Value::List(_) | Value::Outcome(_)) {
            return Err("matches requires a structured value".into());
        }
        let mut report = MatchReport::default();
        self.matches_block(&value, block, "", &mut report)?;
        Ok(report)
    }

    fn matches_block(
        &self,
        value: &Value,
        block: &MatchBlock,
        prefix: &str,
        report: &mut MatchReport,
    ) -> EvalResult<()> {
        let keys = block.exhaustive.then(|| keys(value)).flatten();
        if block.exhaustive && keys.is_none() {
            report.record(
                &block.span,
                qualified(prefix, "matches exhaustive: expected object"),
            );
        }

        for entry in &block.entries {
            let expected = match &entry.pattern {
                MatchPattern::Value(expression) => Some(self.eval(expression)?),
                _ => None,
            };
            let path = qualified(prefix, &entry.path);
            let actual = match self.lookup_path(value.clone(), &entry.path) {
                Ok(Value::Absent) => {
                    report.record(&entry.span, format!("{path}: field is missing"));
                    continue;
                }
                Ok(value) => value,
                Err(error) => {
                    report.record(&entry.span, format!("{path}: {error}"));
                    continue;
                }
            };
            let label = match &entry.pattern {
                MatchPattern::Value(_) => expected
                    .filter(|expected| !equal(&actual, expected))
                    .map(|expected| display(&expected)),
                MatchPattern::Nested(nested) => {
                    self.matches_block(&actual, nested, &path, report)?;
                    None
                }
                MatchPattern::Predicate(MatchPredicate::NotEmpty) if is_empty(&actual) => {
                    Some("non-empty".to_owned())
                }
                MatchPattern::Predicate(MatchPredicate::Null) if actual != Value::Null => {
                    Some("null".to_owned())
                }
                MatchPattern::Predicate(MatchPredicate::Empty)
                    if actual == Value::Null || !is_empty(&actual) =>
                {
                    Some("empty (\"\" / [] / {})".to_owned())
                }
                MatchPattern::Predicate(_) => None,
            };
            if let Some(expected) = label {
                report.record(
                    &entry.span,
                    format!("{path}: expected {expected}, got {}", display(&actual)),
                );
            }
        }

        for key in keys.into_iter().flatten() {
            if !block.entries.iter().any(|entry| entry.path == key) {
                let path = qualified(prefix, key);
                report.record(
                    &block.span,
                    format!("{path}: present in value but unclaimed in exhaustive block"),
                );
            }
        }
        Ok(())
    }
}

fn keys(value: &Value) -> Option<Vec<&str>> {
    match value {
        Value::Outcome(outcome) => {
            let mut keys = vec!["exit_code", "killed", "ok", "signal", "stderr", "stdout"];
            if outcome.pid.is_some() {
                keys.push("pid");
            }
            if outcome.primary != Value::Absent {
                keys.push("value");
            }
            Some(keys)
        }
        value => value
            .record_fields()
            .map(|record| record.keys().map(String::as_str).collect()),
    }
}

fn qualified(prefix: &str, path: &str) -> String {
    if prefix.is_empty() {
        path.to_owned()
    } else {
        format!("{prefix}.{path}")
    }
}

fn display(value: &Value) -> String {
    match value {
        Value::Absent | Value::Null => "null".to_owned(),
        Value::List(items) if items.is_empty() => "[]".to_owned(),
        Value::List(_) => "<list>".to_owned(),
        value
            if value
                .record_fields()
                .is_some_and(|fields| fields.is_empty()) =>
        {
            "{}".to_owned()
        }
        value => value
            .scalar_text()
            .map_or_else(|| "<object>".to_owned(), |text| format!("{text:?}")),
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
