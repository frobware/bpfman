//! Embedded jq evaluation at the shell's effect boundary.

use bpfman_shell_runtime::{QueryInput, QueryRequest, Value};
use jaq_core::{
    Compiler, Ctx, Cv, Error, RunPtr, ValXs, Vars, data,
    load::{Arena, File, Loader},
    native::{Filter, bome, run as native, v},
    unwrap_valr,
};
use jaq_json::{Val, read};
use serde_json::Value as Json;

use super::{bpfman::project, files::to_json};

type Data = data::JustLut<Val>;

pub(super) fn run(request: QueryRequest) -> Result<Vec<Value>, String> {
    let defs = jaq_core::defs()
        .chain(jaq_std::defs())
        .chain(jaq_json::defs())
        .filter(|def| !matches!(def.name, "debug" | "stderr" | "halt_error"));
    let custom: [Filter<RunPtr<Data>>; 3] = [
        ("fromjson", v(0), from_json),
        ("tojson", v(0), serialize_json),
        ("env", v(0), empty_environment),
    ];
    let functions = jaq_core::funs()
        .chain(jaq_std::funs())
        .chain(jaq_json::funs())
        .filter(|(name, _, _)| {
            !matches!(
                *name,
                "fromjson" | "tojson" | "env" | "debug_empty" | "stderr_empty"
            )
        })
        .chain(custom.into_iter().map(native::<Data>));

    let arena = Arena::default();
    let source = File {
        code: request.filter.as_str(),
        path: (),
    };
    let modules = Loader::new(defs)
        .load(&arena, source)
        .map_err(|error| format!("jq: parse filter: {error:?}"))?;
    let filter = Compiler::default()
        .with_funs(functions)
        .compile(modules)
        .map_err(|error| format!("jq: compile filter: {error:?}"))?;

    let input = match request.input {
        QueryInput::JsonText(text) => parse_json(text.as_bytes())
            .map_err(|error| format!("jq: input is not valid JSON: {error}"))?,
        QueryInput::Value(value) => json_to_jaq(&to_json(&value)?)?,
    };
    let context = Ctx::<Data>::new(&filter.lut, Vars::new([]));
    filter
        .id
        .run((context, input))
        .map(unwrap_valr)
        .map(|result| {
            let value = result.map_err(|error| format!("jq: {error}"))?;
            project(jaq_to_json(&value)?)
        })
        .collect()
}

fn parse_json(text: &[u8]) -> Result<Val, String> {
    let value: Json = serde_json::from_slice(text).map_err(|error| error.to_string())?;
    json_to_jaq(&value)
}

fn json_to_jaq(value: &Json) -> Result<Val, String> {
    let text = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    read::parse_single(&text).map_err(|error| error.to_string())
}

fn jaq_to_json(value: &Val) -> Result<Json, String> {
    serde_json::from_str(&value.to_string()).map_err(|error| format!("jq: JSON output: {error}"))
}

fn from_json(cv: Cv<'_, Data>) -> ValXs<'_, Val> {
    let result = match &cv.1 {
        Val::TStr(text) => parse_json(text.as_ref()).ok(),
        _ => None,
    }
    .ok_or_else(|| Error::str(format_args!("fromjson cannot be applied to {}", cv.1)));
    bome(result)
}

fn serialize_json(cv: Cv<'_, Data>) -> ValXs<'_, Val> {
    let result = jaq_to_json(&cv.1)
        .and_then(|value| serde_json::to_string(&value).map_err(|error| error.to_string()))
        .map(Val::from)
        .map_err(Error::str);
    bome(result)
}

fn empty_environment(_: Cv<'_, Data>) -> ValXs<'_, Val> {
    bome(Ok(Val::obj(Default::default())))
}
