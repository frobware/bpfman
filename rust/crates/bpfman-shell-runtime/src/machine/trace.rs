use bpfman_shell_ir::ParameterType;
use bpfman_shell_source::Span;

use super::{Argument, Argv, Event, Machine, Value};

/// The resolved pieces of an execution trace line.
#[derive(Debug, Eq, PartialEq)]
pub enum TracePart {
    /// Literal trace text, including separators.
    Text(String),
    /// An already evaluated command argument.
    Argument(Argument),
    /// An already evaluated expression value.
    Value(Value),
}

/// A source-annotated execution trace observation.
#[derive(Debug, Eq, PartialEq)]
pub struct TraceRequest {
    /// The origin of the traced statement.
    pub span: Span,
    /// Resolved pieces of the trace message.
    pub parts: Vec<TracePart>,
}

impl Machine<'_> {
    pub(super) fn set_trace(&mut self, arguments: &[Argument]) -> Result<(), String> {
        let [argument] = arguments else {
            return Err("trace requires exactly one argument: on or off".to_owned());
        };
        let flag = match argument {
            Argument::Word(text) | Argument::Quoted(text) => text.clone(),
            Argument::Value(value) | Argument::Adapter { value, .. } => {
                value.scalar_text().unwrap_or_default()
            }
        };
        self.trace_enabled = match flag.as_str() {
            "on" => true,
            "off" => false,
            _ => {
                return Err(format!(
                    "trace: unknown argument {flag:?} (expected on or off)"
                ));
            }
        };
        Ok(())
    }

    pub(super) fn trace_definitions(&mut self) {
        if !self.trace_enabled {
            return;
        }
        for def in self.program.definitions() {
            let parameters = def
                .parameters()
                .iter()
                .map(|parameter| {
                    let ty = match parameter.annotation {
                        None => return parameter.name.clone(),
                        Some(ParameterType::Number) => "number",
                        Some(ParameterType::String) => "string",
                        Some(ParameterType::Bool) => "bool",
                    };
                    format!("{}: {ty}", parameter.name)
                })
                .collect::<Vec<_>>()
                .join(" ");
            self.trace_note(def.span(), &format!("def {}({parameters})", def.name()));
        }
    }

    pub(super) fn trace_note(&mut self, span: &Span, text: &str) {
        if self.trace_enabled {
            self.emit_trace(span, vec![TracePart::Text(text.to_owned())]);
        }
    }

    pub(super) fn trace_value(&mut self, span: &Span, prefix: String, value: &Value) {
        if self.trace_enabled {
            self.emit_trace(
                span,
                vec![TracePart::Text(prefix), TracePart::Value(value.clone())],
            );
        }
    }

    pub(super) fn trace_bindings(
        &mut self,
        span: &Span,
        prefix: &str,
        names: &[String],
        value: &Value,
    ) {
        if self.trace_enabled
            && let Value::List(items) = value
            && names.len() == items.len()
        {
            let mut parts = vec![TracePart::Text(prefix.to_owned())];
            for (name, value) in names.iter().zip(items) {
                parts.push(TracePart::Text(format!(" {name}=")));
                parts.push(TracePart::Value(value.clone()));
            }
            self.emit_trace(span, parts);
        }
    }

    pub(super) fn trace_argv(&mut self, span: &Span, prefix: &str, argv: &Argv) {
        if self.trace_enabled {
            let mut parts = vec![
                TracePart::Text(prefix.to_owned()),
                TracePart::Argument(argv.head.clone()),
            ];
            for argument in &argv.arguments {
                parts.push(TracePart::Text(" ".to_owned()));
                parts.push(TracePart::Argument(argument.clone()));
            }
            self.emit_trace(span, parts);
        }
    }

    fn emit_trace(&mut self, span: &Span, parts: Vec<TracePart>) {
        self.output.push_back(Event::Trace(TraceRequest {
            span: span.clone(),
            parts,
        }));
    }
}
