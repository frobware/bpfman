use bpfman_shell_ir::{Expression, PureBuiltin, VariableRef};
use bpfman_shell_semantics::parse_go_unsigned;

use super::{Argument, EvalResult, Machine, Number, Value};

impl Machine<'_> {
    pub(super) fn pure_call(
        &self,
        builtin: PureBuiltin,
        arguments: &[Expression],
    ) -> EvalResult<Value> {
        let arguments = arguments
            .iter()
            .map(|value| self.argument(value))
            .collect::<Result<Vec<_>, _>>()?;
        evaluate(self, builtin, &arguments).map_err(Into::into)
    }
}

pub(super) fn evaluate(
    machine: &Machine<'_>,
    builtin: PureBuiltin,
    arguments: &[Argument],
) -> Result<Value, String> {
    use PureBuiltin as B;
    match (builtin, arguments) {
        (B::Null | B::Present | B::Missing | B::Empty, [arg]) => {
            let value = match arg {
                Argument::Word(path) => {
                    let boundary = path.find(['.', '[']).unwrap_or(path.len());
                    let (name, path) = path.split_at(boundary);
                    if machine.frames.iter().any(|frame| frame.contains_key(name)) {
                        machine.lookup(
                            &VariableRef {
                                name: name.to_owned(),
                                path: path.to_owned(),
                            },
                            true,
                        )?
                    } else {
                        Value::Absent
                    }
                }
                Argument::Quoted(text) => Value::String(text.clone()),
                Argument::Value(value) | Argument::Adapter { value, .. } => value.clone(),
            };
            Ok(Value::Bool(match builtin {
                B::Null => value == Value::Null,
                B::Present => value != Value::Absent,
                B::Missing => value == Value::Absent,
                B::Empty => match value {
                    Value::String(text) => text.is_empty(),
                    Value::List(items) => items.is_empty(),
                    Value::Record(fields) => fields.is_empty(),
                    _ => false,
                },
                _ => return Err("expected a presence predicate".to_owned()),
            }))
        }
        (B::Contains, [haystack, needle]) => {
            Ok(Value::Bool(text(haystack)?.contains(&text(needle)?)))
        }
        (
            B::Zip,
            [
                Argument::Value(Value::List(left)),
                Argument::Value(Value::List(right)),
            ],
        ) => {
            if left.len() != right.len() {
                return Err(format!(
                    "zip: length mismatch (arg 0 has {}, arg 1 has {})",
                    left.len(),
                    right.len()
                ));
            }
            Ok(Value::List(
                left.iter()
                    .zip(right)
                    .map(|(a, b)| Value::List(vec![a.clone(), b.clone()]))
                    .collect(),
            ))
        }
        (B::Zip, [_, _]) => Err("zip requires lists".to_owned()),
        (B::U32Le | B::U64Le | B::Range, [arg]) => {
            let text = text(arg)?;
            let n = parse_go_unsigned(text.trim())
                .ok_or_else(|| format!("invalid unsigned integer {text:?}"))?;
            match builtin {
                B::U32Le => u32::try_from(n)
                    .map(|n| hexadecimal(&n.to_le_bytes()))
                    .map_err(|_| format!("value {n} does not fit in 32 bits")),
                B::U64Le => Ok(hexadecimal(&n.to_le_bytes())),
                B::Range => range(n),
                _ => Err("expected an unsigned builtin".to_owned()),
            }
        }
        (B::Jq | B::PathExists, _) => {
            Err("builtin requires an effect adapter that is not yet implemented".to_owned())
        }
        _ => Err("invalid builtin argument count".to_owned()),
    }
}

fn text(arg: &Argument) -> Result<String, String> {
    match arg {
        Argument::Word(text) | Argument::Quoted(text) => Ok(text.clone()),
        Argument::Value(value) | Argument::Adapter { value, .. } => value
            .scalar_text()
            .ok_or_else(|| "expected a scalar argument".to_owned()),
    }
}

fn hexadecimal(bytes: &[u8]) -> Value {
    Value::String(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn range(n: u64) -> Result<Value, String> {
    if n > i32::MAX as u64 {
        return Err(format!(
            "range: bound {n} exceeds the maximum of {}",
            i32::MAX
        ));
    }
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(n as usize)
        .map_err(|error| format!("range: {error}"))?;
    for index in 0..n {
        let value = Number::new(index.to_string()).ok_or("invalid range index")?;
        entries.push(Value::Number(value));
    }
    Ok(Value::List(entries))
}
