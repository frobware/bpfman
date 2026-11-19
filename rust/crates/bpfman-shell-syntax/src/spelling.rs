//! Canonical source spellings for typed syntax.

use std::fmt::Write as _;

use crate::{BinaryOperator, LogicalOperator, PositiveDuration, PureBuiltin};

pub(crate) const fn binary(operator: BinaryOperator) -> &'static str {
    match operator {
        BinaryOperator::Equal => "==",
        BinaryOperator::NotEqual => "!=",
        BinaryOperator::Less => "<",
        BinaryOperator::LessEqual => "<=",
        BinaryOperator::Greater => ">",
        BinaryOperator::GreaterEqual => ">=",
        BinaryOperator::Add => "+",
        BinaryOperator::Subtract => "-",
        BinaryOperator::Multiply => "*",
        BinaryOperator::Divide => "/",
        BinaryOperator::Remainder => "%",
    }
}

pub(crate) const fn logical(operator: LogicalOperator) -> &'static str {
    match operator {
        LogicalOperator::And => "and",
        LogicalOperator::Or => "or",
    }
}

pub(crate) const fn builtin(builtin: PureBuiltin) -> &'static str {
    match builtin {
        PureBuiltin::Jq => "jq",
        PureBuiltin::U32Le => "u32le",
        PureBuiltin::U64Le => "u64le",
        PureBuiltin::Range => "range",
        PureBuiltin::Zip => "zip",
        PureBuiltin::PathExists => "path-exists",
        PureBuiltin::Contains => "contains",
        PureBuiltin::Null => "null",
        PureBuiltin::Present => "present",
        PureBuiltin::Missing => "missing",
        PureBuiltin::Empty => "empty",
    }
}

pub(crate) fn duration(duration: PositiveDuration) -> String {
    let nanos = duration.as_nanos();
    match nanos {
        0..1_000 => format!("{nanos}ns"),
        1_000..1_000_000 => fractional(nanos / 1_000, nanos % 1_000, 3, "µs"),
        1_000_000..1_000_000_000 => fractional(nanos / 1_000_000, nanos % 1_000_000, 6, "ms"),
        _ => {
            let seconds = nanos / 1_000_000_000;
            let mut out = String::new();
            if seconds >= 3600 {
                let _ = write!(out, "{}h", seconds / 3600);
            }
            if seconds >= 60 {
                let _ = write!(out, "{}m", (seconds / 60) % 60);
            }
            out.push_str(&fractional(seconds % 60, nanos % 1_000_000_000, 9, "s"));
            out
        }
    }
}

fn fractional(integer: u64, fraction: u64, digits: usize, unit: &str) -> String {
    if fraction == 0 {
        return format!("{integer}{unit}");
    }
    let fraction = format!("{fraction:0digits$}");
    format!("{integer}.{}{unit}", fraction.trim_end_matches('0'))
}
