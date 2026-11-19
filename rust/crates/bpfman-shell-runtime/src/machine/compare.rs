use std::cmp::Ordering;

use bpfman_shell_ir::BinaryOperator as Op;
use num_bigint::BigInt;

use super::{Number, Value};

pub(super) fn evaluate(op: Op, left: Value, right: Value) -> Result<Value, String> {
    use Op::{Equal, Greater, GreaterEqual, Less, LessEqual, NotEqual};
    let equality = match op {
        Equal | NotEqual => true,
        Less | LessEqual | Greater | GreaterEqual => false,
        _ => return arithmetic(op, left, right),
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
        (Value::Number(a), Value::Number(b)) => match (integer(a), integer(b)) {
            (Some(a), Some(b)) => a.cmp(&b),
            _ => float(a)?
                .partial_cmp(&float(b)?)
                .ok_or("invalid numeric comparison")?,
        },
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

fn arithmetic(op: Op, left: Value, right: Value) -> Result<Value, String> {
    use Op::{Add, Divide, Multiply, Remainder, Subtract};
    let number = |value: Value, side| match value {
        Value::Number(number) => Ok(number),
        Value::String(text) => Number::new(text)
            .ok_or_else(|| format!("arithmetic {op}: {side} operand is not numeric")),
        _ => Err(format!("arithmetic {op}: {side} operand is not numeric")),
    };
    let left = number(left, "left")?;
    let right = number(right, "right")?;
    let text = if let (Some(a), Some(b), false) = (integer(&left), integer(&right), op == Divide) {
        let result = match op {
            Add => a + b,
            Subtract => a - b,
            Multiply => a * b,
            Remainder if b == BigInt::ZERO => return Err("division by zero".to_owned()),
            Remainder => a % b,
            _ => return Err("expected arithmetic operator".to_owned()),
        };
        result.to_string()
    } else {
        let a = float(&left)?;
        let b = float(&right)?;
        let result = match op {
            Add => a + b,
            Subtract => a - b,
            Multiply => a * b,
            Divide | Remainder if b == 0.0 => return Err("division by zero".to_owned()),
            Divide => a / b,
            Remainder => a % b,
            _ => return Err("expected arithmetic operator".to_owned()),
        };
        if !result.is_finite() {
            return Err(format!(
                "arithmetic {op}: result exceeds the representable range"
            ));
        }
        result.to_string()
    };
    Number::new(text)
        .map(Value::Number)
        .ok_or_else(|| "invalid arithmetic result".to_owned())
}

fn integer(number: &Number) -> Option<BigInt> {
    number.as_str().parse().ok()
}

fn float(number: &Number) -> Result<f64, String> {
    let text = number.as_str();
    text.parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .ok_or_else(|| format!("number {text:?} exceeds the representable range"))
}

#[cfg(test)]
mod tests {
    use super::{Number, Op, Value, evaluate};

    fn number(text: &str) -> Value {
        Value::Number(Number::new(text.to_owned()).unwrap_or_else(|| unreachable!()))
    }

    #[test]
    fn integral_arithmetic_is_exact_and_mixed_arithmetic_promotes() {
        assert_eq!(
            evaluate(
                Op::Add,
                number("999999999999999999999999999999999999999999999999"),
                number("1")
            ),
            Ok(number("1000000000000000000000000000000000000000000000000"))
        );
        assert_eq!(
            evaluate(Op::Remainder, number("-7"), number("3")),
            Ok(number("-1"))
        );
        assert_eq!(
            evaluate(Op::Divide, number("7"), number("2")),
            Ok(number("3.5"))
        );
        assert_eq!(
            evaluate(Op::Divide, number("1"), number("0")),
            Err("division by zero".to_owned())
        );
    }
}
