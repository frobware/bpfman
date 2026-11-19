use std::cmp::Ordering;

use bpfman_shell_ir::BinaryOperator as Op;

use super::{Number, Value};

pub(super) fn evaluate(op: Op, left: Value, right: Value) -> Result<Value, String> {
    use Op::{Equal, Greater, GreaterEqual, Less, LessEqual, NotEqual};
    let equality = match op {
        Equal | NotEqual => true,
        Less | LessEqual | Greater | GreaterEqual => false,
        _ => return Err("arithmetic is not yet implemented".to_owned()),
    };
    let order = match (&left, &right) {
        (Value::Absent, _) | (_, Value::Absent) => {
            return Err("cannot compare an absent value".to_owned());
        }
        (Value::Null, Value::Null) if equality => Ordering::Equal,
        (Value::Null, _) | (_, Value::Null) if equality => Ordering::Less,
        (Value::Null, _) | (_, Value::Null) => {
            return Err("null supports only == and !=".to_owned());
        }
        (Value::String(a), Value::String(b)) => a.cmp(b),
        (Value::Bool(a), Value::Bool(b)) if equality => a.cmp(b),
        (Value::Bool(_), Value::Bool(_)) => {
            return Err("booleans support only == and !=".to_owned());
        }
        (Value::Number(a), Value::Number(b)) => compare_numbers(a, b)?,
        _ => return Err("comparison requires compatible scalar operands".to_owned()),
    };
    Ok(Value::Bool(match op {
        Equal => order.is_eq(),
        NotEqual => order.is_ne(),
        Less => order.is_lt(),
        LessEqual => order.is_le(),
        Greater => order.is_gt(),
        GreaterEqual => order.is_ge(),
        _ => return Err("expected comparison operator".to_owned()),
    }))
}

fn compare_numbers(left: &Number, right: &Number) -> Result<Ordering, String> {
    let left = left.as_str();
    let right = right.as_str();
    if !left.contains(['.', 'e', 'E']) && !right.contains(['.', 'e', 'E']) {
        let (left_negative, left) = integer_parts(left);
        let (right_negative, right) = integer_parts(right);
        return Ok(match (left_negative, right_negative) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => left.len().cmp(&right.len()).then_with(|| left.cmp(right)),
            (true, true) => right.len().cmp(&left.len()).then_with(|| right.cmp(left)),
        });
    }
    let finite = |text: &str| {
        text.parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .ok_or_else(|| format!("number {text:?} exceeds the representable range"))
    };
    finite(left)?
        .partial_cmp(&finite(right)?)
        .ok_or_else(|| "invalid numeric comparison".to_owned())
}

fn integer_parts(text: &str) -> (bool, &str) {
    let magnitude = text.strip_prefix('-').unwrap_or(text);
    (magnitude != "0" && magnitude != text, magnitude)
}
