use bpfman_shell_ir::{self as ir, BinaryOperator, Expression, ExpressionKind};
use bpfman_shell_syntax as syntax;

pub(super) fn lower_expressions(values: &[syntax::Expression]) -> Vec<Expression> {
    values.iter().map(lower_expression).collect()
}

pub(super) fn lower_expression(value: &syntax::Expression) -> Expression {
    use syntax::ExpressionKind as E;
    let node = match value.kind() {
        E::Word(word) => ExpressionKind::Word(word.clone()),
        E::Quoted(word) => ExpressionKind::Quoted(word.clone()),
        E::Variable(variable) => ExpressionKind::Variable(lower_variable(variable)),
        E::Adapter { name, variable } => ExpressionKind::Adapter {
            name: name.clone(),
            variable: lower_variable(variable),
        },
        E::Interpolated(parts) => ExpressionKind::Interpolated(
            parts
                .iter()
                .map(|part| match part {
                    syntax::InterpolatedPart::Literal(text) => {
                        ir::InterpolatedPart::Literal(text.clone())
                    }
                    syntax::InterpolatedPart::Expression(value) => {
                        ir::InterpolatedPart::Expression(lower_expression(value))
                    }
                })
                .collect(),
        ),
        E::Record(fields) => ExpressionKind::Record(
            fields
                .iter()
                .map(|field| ir::RecordField {
                    name: field.name.text().to_owned(),
                    value: lower_expression(&field.value),
                    span: field.span.clone(),
                })
                .collect(),
        ),
        E::Logical {
            left,
            operator,
            right,
        } => ExpressionKind::Logical {
            left: Box::new(lower_expression(left)),
            operator: match operator {
                syntax::LogicalOperator::And => ir::LogicalOperator::And,
                syntax::LogicalOperator::Or => ir::LogicalOperator::Or,
            },
            right: Box::new(lower_expression(right)),
        },
        E::Not(operand) => unary(ir::UnaryOperator::Not, operand),
        E::Negate(operand) => unary(ir::UnaryOperator::Negate, operand),
        E::NotEmpty(operand) => unary(ir::UnaryOperator::NotEmpty, operand),
        E::PureCall { builtin, arguments } => ExpressionKind::PureCall {
            builtin: lower_builtin(*builtin),
            arguments: lower_expressions(arguments),
        },
        E::Thread {
            value,
            command,
            pipe,
        } => ExpressionKind::Thread {
            value: Box::new(lower_expression(value)),
            command: Box::new(ir::Command {
                head: lower_expression(command.head()),
                arguments: lower_expressions(command.arguments()),
            }),
            pipe: pipe.clone(),
        },
        E::Matches { target, block } => ExpressionKind::Matches {
            target: Box::new(lower_expression(target)),
            block: lower_match_block(block),
        },
        E::List(items) => ExpressionKind::List(lower_expressions(items)),
        E::Binary {
            left,
            operator,
            right,
        } => ExpressionKind::Binary {
            left: Box::new(lower_expression(left)),
            operator: lower_operator(*operator),
            right: Box::new(lower_expression(right)),
        },
    };
    ir::Located {
        node,
        span: value.span().clone(),
    }
}

fn lower_operator(operator: syntax::BinaryOperator) -> BinaryOperator {
    use syntax::BinaryOperator as O;
    match operator {
        O::Add => BinaryOperator::Add,
        O::Subtract => BinaryOperator::Subtract,
        O::Multiply => BinaryOperator::Multiply,
        O::Divide => BinaryOperator::Divide,
        O::Remainder => BinaryOperator::Remainder,
        O::Equal => BinaryOperator::Equal,
        O::NotEqual => BinaryOperator::NotEqual,
        O::Less => BinaryOperator::Less,
        O::LessEqual => BinaryOperator::LessEqual,
        O::Greater => BinaryOperator::Greater,
        O::GreaterEqual => BinaryOperator::GreaterEqual,
    }
}

fn lower_variable(variable: &syntax::VariableRef) -> ir::VariableRef {
    ir::VariableRef {
        name: variable.name.clone(),
        path: variable.path.clone(),
    }
}

fn unary(operator: ir::UnaryOperator, operand: &syntax::Expression) -> ExpressionKind {
    ExpressionKind::Unary {
        operator,
        operand: Box::new(lower_expression(operand)),
    }
}

fn lower_match_block(block: &syntax::MatchesBlock) -> ir::MatchBlock {
    ir::MatchBlock {
        entries: block
            .entries
            .iter()
            .map(|entry| ir::MatchEntry {
                path: entry.path.clone(),
                span: entry.span.clone(),
                pattern: match &entry.pattern {
                    syntax::MatchPattern::Value(value) => {
                        ir::MatchPattern::Value(lower_expression(value))
                    }
                    syntax::MatchPattern::Nested(block) => {
                        ir::MatchPattern::Nested(lower_match_block(block))
                    }
                    syntax::MatchPattern::Predicate(predicate) => {
                        ir::MatchPattern::Predicate(match predicate {
                            syntax::MatchPredicate::NotEmpty => ir::MatchPredicate::NotEmpty,
                            syntax::MatchPredicate::Null => ir::MatchPredicate::Null,
                            syntax::MatchPredicate::Empty => ir::MatchPredicate::Empty,
                        })
                    }
                },
            })
            .collect(),
        exhaustive: block.exhaustive,
        span: block.span.clone(),
    }
}

fn lower_builtin(builtin: syntax::PureBuiltin) -> ir::PureBuiltin {
    use syntax::PureBuiltin as B;

    match builtin {
        B::Jq => ir::PureBuiltin::Jq,
        B::U32Le => ir::PureBuiltin::U32Le,
        B::U64Le => ir::PureBuiltin::U64Le,
        B::Range => ir::PureBuiltin::Range,
        B::Zip => ir::PureBuiltin::Zip,
        B::PathExists => ir::PureBuiltin::PathExists,
        B::Contains => ir::PureBuiltin::Contains,
        B::Null => ir::PureBuiltin::Null,
        B::Present => ir::PureBuiltin::Present,
        B::Missing => ir::PureBuiltin::Missing,
        B::Empty => ir::PureBuiltin::Empty,
    }
}
