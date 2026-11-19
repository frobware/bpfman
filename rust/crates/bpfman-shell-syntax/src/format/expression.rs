//! Canonical expression spelling in each grammar position.

use crate::{
    Expression, ExpressionKind as Kind, InterpolatedPart, MatchEntry, MatchPattern, MatchPredicate,
    MatchesBlock, VariableRef,
    spelling::{binary, builtin, logical},
};

use super::indent;

#[derive(Clone, Copy)]
pub(super) enum Position {
    Top,
    Atom,
    CommandArgument,
    ListElement,
    FunctionArgument,
}

pub(super) fn write(
    out: &mut String,
    expression: &Expression,
    position: Position,
    depth: Option<usize>,
) {
    let needs_group = match position {
        Position::Top => false,
        Position::Atom => compound(expression.kind()),
        Position::CommandArgument => {
            compound(expression.kind())
                || matches!(expression.kind(), Kind::PureCall { .. } | Kind::Record(_))
        }
        Position::ListElement => compound(expression.kind()),
        Position::FunctionArgument => {
            compound(expression.kind()) || matches!(expression.kind(), Kind::PureCall { .. })
        }
    };
    if needs_group {
        out.push('(');
    }
    match expression.kind() {
        Kind::Word(word) => out.push_str(word),
        Kind::Quoted(text) => quoted(out, text),
        Kind::Variable(reference) => variable(out, reference, true),
        Kind::Adapter {
            name,
            variable: reference,
        } => {
            out.push_str(name);
            out.push(':');
            variable(out, reference, true);
        }
        Kind::Interpolated(parts) => {
            out.push('"');
            for part in parts {
                match part {
                    InterpolatedPart::Literal(text) => double_quoted(out, text),
                    InterpolatedPart::Expression(expression) => {
                        out.push('$');
                        out.push('{');
                        match expression.kind() {
                            Kind::Variable(reference) => variable(out, reference, false),
                            Kind::Adapter {
                                name,
                                variable: reference,
                            } => {
                                out.push_str(name);
                                out.push(':');
                                variable(out, reference, false);
                            }
                            _ => write(out, expression, Position::Top, None),
                        }
                        out.push('}');
                    }
                }
            }
            out.push('"');
        }
        Kind::Binary {
            left,
            operator,
            right,
        } => infix(out, left, binary(*operator), right),
        Kind::Logical {
            left,
            operator,
            right,
        } => infix(out, left, logical(*operator), right),
        Kind::Not(operand) => prefix(out, "not ", operand),
        Kind::NotEmpty(operand) => prefix(out, "not-empty ", operand),
        Kind::Negate(operand) => prefix(out, "-", operand),
        Kind::Thread { value, command, .. } => {
            write(out, value, Position::Atom, None);
            out.push_str(" |> ");
            write(out, command.head(), Position::Top, None);
            for argument in command.arguments() {
                out.push(' ');
                write(out, argument, Position::Top, None);
            }
        }
        Kind::PureCall {
            builtin: function,
            arguments,
        } => {
            out.push_str(builtin(*function));
            for argument in arguments {
                out.push(' ');
                write(out, argument, Position::FunctionArgument, None);
            }
        }
        Kind::List(elements) => {
            out.push('[');
            for (index, element) in elements.iter().enumerate() {
                if index != 0 {
                    out.push(' ');
                }
                write(out, element, Position::ListElement, None);
            }
            out.push(']');
        }
        Kind::Record(fields) => {
            out.push_str("record {");
            for (index, field) in fields.iter().enumerate() {
                if index != 0 {
                    out.push(' ');
                }
                out.push_str(field.name.text());
                out.push_str(": ");
                write(out, &field.value, Position::ListElement, None);
            }
            out.push('}');
        }
        Kind::Matches { target, block } => {
            write(out, target, Position::Atom, None);
            out.push(' ');
            matches_block(out, block, depth);
        }
    }
    if needs_group {
        out.push(')');
    }
}

fn compound(kind: &Kind) -> bool {
    matches!(
        kind,
        Kind::Binary { .. }
            | Kind::Logical { .. }
            | Kind::Thread { .. }
            | Kind::Not(_)
            | Kind::NotEmpty(_)
            | Kind::Negate(_)
            | Kind::Matches { .. }
    )
}

fn infix(out: &mut String, left: &Expression, operator: &str, right: &Expression) {
    write(out, left, Position::Atom, None);
    out.push(' ');
    out.push_str(operator);
    out.push(' ');
    write(out, right, Position::Atom, None);
}

fn prefix(out: &mut String, operator: &str, expression: &Expression) {
    out.push_str(operator);
    write(out, expression, Position::Atom, None);
}

fn variable(out: &mut String, reference: &VariableRef, sigil: bool) {
    if sigil {
        out.push('$');
    }
    out.push_str(&reference.name);
    if !reference.path.is_empty() {
        if !reference.path.starts_with('[') {
            out.push('.');
        }
        out.push_str(&reference.path);
    }
}

fn quoted(out: &mut String, value: &str) {
    if value.contains(['"', '$']) && !value.contains(['\'', '\n', '\r', '\t']) {
        out.push('\'');
        out.push_str(value);
        out.push('\'');
    } else {
        out.push('"');
        double_quoted(out, value);
        out.push('"');
    }
}

fn double_quoted(out: &mut String, value: &str) {
    for character in value.chars() {
        out.push_str(match character {
            '\n' => "\\n",
            '\r' => "\\r",
            '\t' => "\\t",
            '\\' => "\\\\",
            '"' => "\\\"",
            '$' => "\\$",
            character => {
                out.push(character);
                continue;
            }
        });
    }
}

fn matches_block(out: &mut String, block: &MatchesBlock, depth: Option<usize>) {
    out.push_str(if block.exhaustive {
        "matches exhaustive {"
    } else {
        "matches {"
    });
    if let Some(depth) = depth {
        if block.entries.is_empty() {
            out.push_str(" }");
            return;
        }
        let width = block
            .entries
            .iter()
            .map(|entry| entry.path.len())
            .max()
            .unwrap_or(0);
        out.push('\n');
        for entry in &block.entries {
            indent(out, depth + 1);
            match_entry(out, entry, Some(depth + 1), width);
            out.push('\n');
        }
        indent(out, depth);
    } else if !block.entries.is_empty() {
        for (index, entry) in block.entries.iter().enumerate() {
            if index != 0 {
                out.push('\n');
            }
            out.push(' ');
            match_entry(out, entry, None, 0);
        }
        out.push(' ');
    }
    out.push('}');
}

fn match_entry(out: &mut String, entry: &MatchEntry, depth: Option<usize>, width: usize) {
    out.push_str(&entry.path);
    out.push_str(": ");
    if !matches!(entry.pattern, MatchPattern::Nested(_)) {
        for _ in entry.path.len()..width {
            out.push(' ');
        }
    }
    match &entry.pattern {
        MatchPattern::Predicate(predicate) => out.push_str(match predicate {
            MatchPredicate::NotEmpty => "not-empty",
            MatchPredicate::Null => "null",
            MatchPredicate::Empty => "empty",
        }),
        MatchPattern::Nested(block) => matches_block(out, block, depth),
        MatchPattern::Value(value) => write(out, value, Position::Top, None),
    }
}
