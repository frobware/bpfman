//! Preflight for literal operands of binary arithmetic.

use bpfman_shell_syntax::{BinaryOperator, Expression, ExpressionKind, is_json_number};

use crate::Issue;

pub(super) fn check(
    operator: BinaryOperator,
    operands: [&Expression; 2],
    is_definition: impl Fn(&str) -> bool,
    variable_issue: impl Fn(&Expression) -> Option<String>,
    issues: &mut Vec<Issue>,
) {
    let op = match operator {
        BinaryOperator::Add => "+",
        BinaryOperator::Subtract => "-",
        BinaryOperator::Multiply => "*",
        BinaryOperator::Divide => "/",
        BinaryOperator::Remainder => "%",
        BinaryOperator::Equal
        | BinaryOperator::NotEqual
        | BinaryOperator::Less
        | BinaryOperator::LessEqual
        | BinaryOperator::Greater
        | BinaryOperator::GreaterEqual => return,
    };

    for operand in operands {
        let text = match operand.kind() {
            ExpressionKind::Word(text) | ExpressionKind::Quoted(text) => text,
            _ => {
                if let Some(reason) = variable_issue(operand) {
                    issues.push(Issue {
                        span: operand.span().clone(),
                        message: format!("arithmetic {op}: {reason}"),
                    });
                }
                continue;
            }
        };
        if is_json_number(text) {
            continue;
        }

        let mut message = format!("arithmetic {op}: operand {text:?} is not numeric");
        if matches!(operand.kind(), ExpressionKind::Word(_)) && is_definition(text) {
            message.push_str(&format!(
                "; {text:?} is a def -- call it via `let v <- {text}` and use $v in the expression"
            ));
        }
        issues.push(Issue {
            span: operand.span().clone(),
            message,
        });
    }
}
