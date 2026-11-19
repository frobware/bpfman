use bpfman_shell_ir::{
    BinaryOperator, Expression, ExpressionKind, LogicalOperator, PureBuiltin, UnaryOperator,
};
use bpfman_shell_source::Span;

use super::{EvalResult, Machine, Value, boolean, compare, location};

pub(super) fn label(required: bool) -> &'static str {
    if required { "require" } else { "assert" }
}

impl Machine<'_> {
    pub(super) fn assert_expression(
        &mut self,
        expression: &Expression,
        required: bool,
        span: &Span,
    ) -> EvalResult<()> {
        let label = label(required);
        if matches!(&expression.node, ExpressionKind::Word(word) if word == "null") {
            return Err(format!(
                "{label} null requires a target; did you mean {label} null $x or {label} $x == null?"
            ).into());
        }
        let reason = self.assertion_failure(expression)?;
        if self.trace_enabled {
            self.trace_note(span, &format!("{label} {expression}"));
        }
        if let Some(reason) = reason {
            self.assertion_result(false, required, reason, span);
        }
        Ok(())
    }

    fn assertion_failure(&self, expression: &Expression) -> EvalResult<Option<String>> {
        match &expression.node {
            ExpressionKind::Matches { target, block } => {
                let report = self.matches(self.eval(target)?, block)?;
                Ok((!report.passed()).then(|| report.describe()))
            }
            ExpressionKind::Logical {
                left,
                operator,
                right,
            } => {
                let left = self.assertion_failure(left)?;
                match operator {
                    LogicalOperator::And if left.is_some() => Ok(left),
                    LogicalOperator::Or if left.is_none() => Ok(None),
                    LogicalOperator::And => self.assertion_failure(right),
                    LogicalOperator::Or => Ok(self.assertion_failure(right)?.and(left)),
                }
            }
            ExpressionKind::Binary {
                left,
                operator,
                right,
            } => {
                let left = self.eval(left)?;
                let right = self.eval(right)?;
                if boolean(compare::evaluate(*operator, left.clone(), right.clone())?)? {
                    return Ok(None);
                }
                let (left, right) = (describe(&left), describe(&right));
                let text = match operator {
                    BinaryOperator::Equal => format!("expected {left:?} to equal {right:?}"),
                    BinaryOperator::NotEqual => format!("expected {left:?} to not equal {right:?}"),
                    _ => format!("expected {left} {operator} {right}"),
                };
                Ok(Some(text))
            }
            _ => {
                let truth = boolean(self.eval(expression)?)?;
                Ok((!truth).then(|| {
                    predicate_failure(expression, false)
                        .unwrap_or_else(|| format!("expected {expression} to be true"))
                }))
            }
        }
    }

    pub(super) fn assertion_result(
        &mut self,
        truth: bool,
        required: bool,
        reason: String,
        span: &Span,
    ) {
        if truth {
            return;
        }
        let message = format!("[{}] FAIL: {reason}", label(required));
        if required {
            self.error(span, message);
        } else {
            self.assert_failures += 1;
            self.exit_code = 1;
            self.report(span, format!("{}: {message}", location(span)));
        }
    }
}

fn predicate_failure(expression: &Expression, negated: bool) -> Option<String> {
    match &expression.node {
        ExpressionKind::Unary {
            operator: UnaryOperator::Not,
            operand,
        } => predicate_failure(operand, !negated),
        ExpressionKind::PureCall { builtin, arguments } => {
            let not = if negated { "not " } else { "" };
            match (builtin, arguments.as_slice()) {
                (PureBuiltin::Null, [value]) => Some(format!("expected {value} {not}to be null")),
                (PureBuiltin::Present, [value]) => {
                    Some(format!("expected {value} {not}to be present"))
                }
                (PureBuiltin::Missing, [value]) => Some(format!(
                    "expected {value} {not}to be missing from the shape"
                )),
                (PureBuiltin::Empty, [value]) => Some(format!(
                    "expected {value} {not}to be empty (\"\" / [] / {{}})"
                )),
                (PureBuiltin::PathExists, [path]) => {
                    Some(format!("expected path {path} {not}to exist"))
                }
                (PureBuiltin::Contains, [haystack, needle]) => {
                    Some(format!("expected {haystack} {not}to contain {needle}"))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn describe(value: &Value) -> String {
    value.scalar_text().unwrap_or_else(|| match value {
        Value::List(_) => "<list>".to_owned(),
        _ => "<object>".to_owned(),
    })
}
