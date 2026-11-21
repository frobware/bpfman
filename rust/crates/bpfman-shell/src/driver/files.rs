use std::{collections::BTreeMap, io::Write};

use bpfman_shell_runtime::{Argument, Value};
use serde_json::{Map, Value as Json};
use tempfile::{Builder, NamedTempFile};

pub(super) fn render(value: &Value, pretty: bool) -> Result<String, String> {
    if let Some(value) = value.scalar_text() {
        return Ok(value);
    }
    let json = to_json(value)?;
    if pretty {
        serde_json::to_string_pretty(&json)
    } else {
        serde_json::to_string(&json)
    }
    .map_err(|error| error.to_string())
}

pub(super) fn render_argument(argument: &Argument, pretty: bool) -> Result<String, String> {
    match argument {
        Argument::Word(text) | Argument::Quoted(text) => Ok(text.clone()),
        Argument::Value(value) | Argument::Adapter { value, .. } => render(value, pretty),
    }
}

fn file_contents(argument: &Argument) -> Result<String, String> {
    let mut contents = render_argument(argument, true)?;
    if let Argument::Value(value) | Argument::Adapter { value, .. } = argument
        && (value.record_fields().is_some() || matches!(value, Value::List(_) | Value::Outcome(_)))
    {
        contents.push('\n');
    }
    Ok(contents)
}

#[derive(Default)]
pub(super) struct AdapterFiles(Vec<NamedTempFile>);

impl AdapterFiles {
    pub(super) fn argument(&mut self, argument: &Argument) -> Result<String, String> {
        if let Argument::Adapter { name, .. } = argument {
            if name != "file" {
                return Err(format!("unknown adapter {name:?}"));
            }
            let file = new_file(argument)?;
            let path = file.path().to_string_lossy().into_owned();
            self.0.push(file);
            Ok(path)
        } else {
            super::argument(argument)
        }
    }
}

impl Drop for AdapterFiles {
    fn drop(&mut self) {
        for file in self.0.drain(..) {
            remove_adapter_file(file);
        }
    }
}

fn remove_adapter_file(file: NamedTempFile) {
    let path = file.path().to_owned();
    if let Err(error) = file.close()
        && error.kind() != std::io::ErrorKind::NotFound
    {
        super::report_cleanup(
            "remove adapter file",
            Err::<(), _>(format!("{}: {error}", path.display())),
        );
    }
}

pub(super) fn tempdir(arguments: &[Argument]) -> Result<Value, String> {
    let [prefix] = arguments else {
        return Err("tempdir requires exactly one prefix".to_owned());
    };
    let prefix = super::argument(prefix)?;
    let prefix = prefix.trim();
    if prefix.is_empty() || prefix.contains('/') {
        return Err("tempdir requires a nonempty filename prefix".to_owned());
    }
    let dir = Builder::new()
        .prefix(&format!("{prefix}."))
        .tempdir()
        .map_err(|error| format!("tempdir: {error}"))?;
    let path = dir.keep().to_string_lossy().into_owned();
    Ok(Value::Record(BTreeMap::from([(
        "path".to_owned(),
        Value::String(path),
    )])))
}

pub(super) fn tempfile(arguments: &[Argument]) -> Result<Value, String> {
    let [command, value] = arguments else {
        return Err("usage: file temp VALUE".to_owned());
    };
    if super::argument(command)? != "temp" {
        return Err("usage: file temp VALUE".to_owned());
    }
    let (_, path) = new_file(value)?
        .keep()
        .map_err(|error| format!("file temp: {error}"))?;
    Ok(Value::String(path.to_string_lossy().into_owned()))
}

fn new_file(argument: &Argument) -> Result<NamedTempFile, String> {
    let contents = file_contents(argument)?;
    let mut file = Builder::new()
        .prefix("bpfman-driver-")
        .tempfile()
        .map_err(|error| format!("create temp file: {error}"))?;
    if let Err(error) = file.write_all(contents.as_bytes()) {
        remove_adapter_file(file);
        return Err(format!("write temp file: {error}"));
    }
    Ok(file)
}

pub(super) fn to_json(value: &Value) -> Result<Json, String> {
    Ok(match value {
        Value::Absent | Value::Null => Json::Null,
        Value::Bool(value) => Json::Bool(*value),
        Value::Number(value) => Json::Number(
            value
                .as_str()
                .parse()
                .map_err(|error| format!("render number: {error}"))?,
        ),
        Value::String(value) => Json::String(value.clone()),
        Value::List(items) => Json::Array(items.iter().map(to_json).collect::<Result<_, _>>()?),
        Value::Outcome(outcome) => {
            let mut fields = Map::from_iter([
                ("ok".to_owned(), Json::Bool(outcome.exit_code == 0)),
                ("exit_code".to_owned(), Json::from(outcome.exit_code)),
                ("stdout".to_owned(), Json::String(outcome.stdout.clone())),
                ("stderr".to_owned(), Json::String(outcome.stderr.clone())),
                ("killed".to_owned(), Json::Bool(outcome.killed)),
                (
                    "signal".to_owned(),
                    Json::String(outcome.signal.clone().unwrap_or_default()),
                ),
            ]);
            if let Some(pid) = outcome.pid {
                fields.insert("pid".to_owned(), Json::from(pid));
            }
            if outcome.primary != Value::Absent {
                fields.insert("value".to_owned(), to_json(&outcome.primary)?);
            }
            Json::Object(fields)
        }
        record => json_object(
            record
                .record_fields()
                .ok_or("value has no record projection")?,
        )?,
    })
}

fn json_object(fields: &BTreeMap<String, Value>) -> Result<Json, String> {
    fields
        .iter()
        .map(|(key, value)| to_json(value).map(|value| (key.clone(), value)))
        .collect::<Result<_, _>>()
        .map(Json::Object)
}
