//! Static operand rules for equality and ordering.

use bpfman_shell_semantics::{OriginKind, Shape};
use bpfman_shell_syntax::{BinaryOperator, Expression, ExpressionKind};

use crate::Issue;

/// The spelling and whether the operator requires ordered operands.
fn comparison(operator: BinaryOperator) -> Option<(&'static str, bool)> {
    match operator {
        BinaryOperator::Equal => Some(("==", false)),
        BinaryOperator::NotEqual => Some(("!=", false)),
        BinaryOperator::Less => Some(("<", true)),
        BinaryOperator::LessEqual => Some(("<=", true)),
        BinaryOperator::Greater => Some((">", true)),
        BinaryOperator::GreaterEqual => Some((">=", true)),
        BinaryOperator::Add
        | BinaryOperator::Subtract
        | BinaryOperator::Multiply
        | BinaryOperator::Divide
        | BinaryOperator::Remainder => None,
    }
}

pub(super) fn issue(
    expression: &Expression,
    infer: impl Fn(&Expression) -> Shape,
    is_definition: impl Fn(&str) -> bool,
) -> Option<Issue> {
    let ExpressionKind::Binary {
        operator,
        left,
        right,
    } = expression.kind()
    else {
        return None;
    };
    let (op, ordered) = comparison(*operator)?;

    for operand in [left, right] {
        if let ExpressionKind::Word(name) = operand.kind() {
            if is_definition(name) {
                return Some(Issue {
                    span: operand.span().clone(),
                    message: format!(
                        "binary {op}: operand {name:?} is a def -- call it via `let v <- {name}` and use $v in the comparison"
                    ),
                });
            }
        }
    }

    let left = comparison_kind(&infer(left));
    let right = comparison_kind(&infer(right));
    let mismatch = mismatch(op, ordered, left, right)?;
    Some(Issue {
        span: expression.span().clone(),
        message: format!("binary {op}: {mismatch}"),
    })
}

fn comparison_kind(shape: &Shape) -> OriginKind {
    match shape {
        Shape::Record {
            origin: OriginKind::Unknown,
            ..
        } => OriginKind::Map,
        _ => shape.origin(),
    }
}

fn mismatch(op: &str, ordered: bool, left: OriginKind, right: OriginKind) -> Option<String> {
    use OriginKind as K;

    if left == K::Unknown || right == K::Unknown {
        return None;
    }
    if left == K::Null || right == K::Null {
        return ordered.then(|| format!("null supports only == and !=, not {op}"));
    }
    for (side, kind) in [("left", left), ("right", right)] {
        if !matches!(kind, K::Scalar | K::Bool) {
            return Some(format!(
                "{side} side has kind {kind}; only scalars (numbers, strings, booleans) can be compared with {op}"
            ));
        }
    }
    if left != right {
        return Some(format!(
            "cannot compare {left} to {right}; coerce explicitly"
        ));
    }
    (left == K::Bool && ordered).then(|| format!("booleans support only == and !=, not {op}"))
}
