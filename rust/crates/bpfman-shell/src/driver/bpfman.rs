use std::{collections::BTreeMap, env, os::unix::process::ExitStatusExt, process::Command};

use bpfman_shell_runtime::{Argument, CommandResponse, Number, Value};
use serde_json::Value as Json;

pub(super) fn invoke(arguments: Vec<Argument>) -> Result<CommandResponse, String> {
    let mut argv = arguments
        .into_iter()
        .map(|argument| match argument {
            Argument::Value(Value::BpfmanRecord { id, .. }) => Ok(id.as_str().to_owned()),
            argument => super::argument(&argument),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if argv.is_empty() {
        return Err("missing command after bpfman".to_owned());
    }
    let output_format = argv.iter().any(|word| {
        matches!(word.as_str(), "-o" | "--output")
            || word.starts_with("-o=")
            || word.starts_with("--output=")
    });
    let noun = argv.first().map(String::as_str).unwrap_or_default();
    let verb = argv.get(1).map(String::as_str).unwrap_or_default();
    let format_supported = !matches!(
        (noun, verb),
        ("image", "build" | "inspect")
            | ("program", "unload" | "delete")
            | ("link", "detach" | "delete")
    );
    let shape = match (noun, verb) {
        ("program", "get") => Shape::Program,
        ("link", "get" | "attach") => Shape::Link,
        ("program", "list" | "load")
        | ("link", "list")
        | ("dispatcher", "get" | "list")
        | ("image", "inspect") => Shape::Json,
        _ => Shape::Void,
    };
    if format_supported && !output_format {
        argv.extend(["-o".to_owned(), "json".to_owned()]);
    }

    let executable = env::var_os("BPFMAN_BIN")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "bpfman".into());
    let output = Command::new(&executable)
        .args(argv)
        .output()
        .map_err(|error| format!("{}: {error}", executable.to_string_lossy()))?;
    let primary = if output.status.success() {
        shape.decode(&output.stdout)?
    } else {
        Value::Absent
    };
    let signal = output.status.signal();
    let code = output
        .status
        .code()
        .unwrap_or_else(|| 128 + signal.unwrap_or(0));
    Ok(CommandResponse {
        exit_code: u8::try_from(code).unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        primary,
        killed: false,
        signal: signal.map(|value| value.to_string()),
        pid: None,
    })
}

#[derive(Clone, Copy)]
enum Shape {
    Void,
    Json,
    Program,
    Link,
}

impl Shape {
    fn decode(self, stdout: &[u8]) -> Result<Value, String> {
        if matches!(self, Self::Void) {
            return Ok(Value::Absent);
        }
        let json: Json = serde_json::from_slice(stdout)
            .map_err(|error| format!("decode bpfman output: {error}"))?;
        let id_key = match self {
            Self::Program => "program_id",
            Self::Link => "id",
            _ => return project(json),
        };
        let id = json
            .get("record")
            .and_then(|record| record.get(id_key))
            .and_then(Json::as_number)
            .and_then(|number| Number::new(number.to_string()))
            .ok_or_else(|| format!("bpfman output has no numeric record.{id_key}"))?;
        let Value::Record(fields) = project(json)? else {
            return Err("expected a bpfman record".to_owned());
        };
        Ok(Value::BpfmanRecord { fields, id })
    }
}

pub(super) fn project(json: Json) -> Result<Value, String> {
    Ok(match json {
        Json::Null => Value::Null,
        Json::Bool(value) => Value::Bool(value),
        Json::Number(value) => {
            Value::Number(Number::new(value.to_string()).ok_or("invalid JSON number")?)
        }
        Json::String(value) => Value::String(value),
        Json::Array(values) => {
            Value::List(values.into_iter().map(project).collect::<Result<_, _>>()?)
        }
        Json::Object(fields) => Value::Record(
            fields
                .into_iter()
                .map(|(name, value)| project(value).map(|value| (name, value)))
                .collect::<Result<BTreeMap<_, _>, _>>()?,
        ),
    })
}
