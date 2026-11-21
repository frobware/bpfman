use std::collections::HashMap;

use bpfman_shell_ir::{BlockId, Expression, ParameterType};
use bpfman_shell_source::{Position, Span};

use super::{
    Argument, CommandPurpose, CommandRequest, CommandResponse, ExecutionError, Invocation, Machine,
    Number, OutputRequest, OutputStream, Slot, Value, loops,
};

const MAX_DEPTH: usize = 256;

pub(super) struct Caller {
    unit: Option<usize>,
    block: BlockId,
    offset: usize,
    temps: Vec<Option<Slot>>,
    pub(super) frame_depth: usize,
    loops: Vec<loops::Iteration>,
    polls: Vec<super::polls::Poll>,
    attempt_cleanup: Option<bpfman_shell_source::Span>,
    pub(super) deferred_base: usize,
    draining: Option<usize>,
    returned: Value,
    cleanup_failed: bool,
    prior_failure: Option<ExecutionError>,
    pending_retry: Option<super::polls::RetrySignal>,
    observations: HashMap<*const Expression, Value>,
    purpose: CommandPurpose,
    call_span: Span,
    invocation: Invocation,
}

impl Machine<'_> {
    pub(super) fn start_call(
        &mut self,
        request: &CommandRequest,
        purpose: CommandPurpose,
    ) -> Result<Option<CommandPurpose>, String> {
        let head = match &request.head {
            Argument::Word(name)
            | Argument::Quoted(name)
            | Argument::Value(Value::String(name)) => name,
            _ => return Ok(Some(purpose)),
        };
        let Some((index, def)) = self
            .program
            .definitions()
            .iter()
            .enumerate()
            .find(|(_, def)| def.name() == head)
        else {
            return self.session_call(head, request, purpose);
        };
        if self.callers.len() >= MAX_DEPTH {
            return Err(format!(
                "in def {head}: recursion depth limit exceeded ({MAX_DEPTH})"
            ));
        }
        if request.arguments.len() != def.parameters().len() {
            return Err(format!(
                "{head}: expected {} argument(s), got {}",
                def.parameters().len(),
                request.arguments.len()
            ));
        }
        let mut temps = Vec::with_capacity(def.num_temps());
        for (arg, param) in request.arguments.iter().zip(def.parameters()) {
            let value = parameter(arg, param.annotation).ok_or_else(|| {
                let wanted = param.annotation.map_or("scalar or record", parameter_type);
                format!(
                    "def {head}: parameter {:?}: expected {wanted}, got {}",
                    param.name,
                    argument_kind(arg)
                )
            })?;
            temps.push(Some(Slot::Value(value)));
        }
        temps.resize_with(def.num_temps(), || None);
        self.callers.push(Caller {
            unit: self.unit.replace(index),
            block: self.block,
            offset: self.offset,
            temps: std::mem::replace(&mut self.temps, temps),
            frame_depth: self.frames.len(),
            loops: std::mem::take(&mut self.loops),
            polls: std::mem::take(&mut self.polls),
            attempt_cleanup: self.attempt_cleanup.take(),
            deferred_base: self.deferred.len(),
            draining: self.draining.take(),
            returned: std::mem::replace(&mut self.returned, Value::Absent),
            cleanup_failed: std::mem::take(&mut self.local_cleanup_failed),
            prior_failure: self.failure.take(),
            pending_retry: self.pending_retry.take(),
            observations: std::mem::take(&mut self.expression_observations),
            purpose,
            call_span: request.span.clone(),
            invocation: Invocation::from_request(request),
        });
        self.jump(BlockId(0));
        Ok(None)
    }

    fn session_call(
        &mut self,
        head: &str,
        request: &CommandRequest,
        purpose: CommandPurpose,
    ) -> Result<Option<CommandPurpose>, String> {
        use bpfman_shell_ir::PureBuiltin;
        let builtin = match head {
            "range" => Some(PureBuiltin::Range),
            "zip" => Some(PureBuiltin::Zip),
            "u32le" => Some(PureBuiltin::U32Le),
            "u64le" => Some(PureBuiltin::U64Le),
            _ => None,
        };
        if let Some(builtin) = builtin {
            let value = super::pure::evaluate(self, builtin, &request.arguments)?;
            self.complete_command(
                purpose,
                &Invocation::from_request(request),
                Ok(CommandResponse::success(value)),
            );
            return Ok(None);
        }
        if head == "trace" {
            self.set_trace(&request.arguments)?;
            self.complete_command(
                purpose,
                &Invocation::from_request(request),
                Ok(CommandResponse::success(Value::Absent)),
            );
            return Ok(None);
        }
        if head != "defs" {
            return Ok(Some(purpose));
        }
        let mut signatures = self.program.definitions().iter().collect::<Vec<_>>();
        signatures.sort_unstable_by_key(|def| def.name());
        if !signatures.is_empty() {
            let listing = signatures
                .into_iter()
                .map(|def| {
                    let names = def
                        .parameters()
                        .iter()
                        .map(|param| param.name.as_str())
                        .collect::<Vec<_>>();
                    format!("  {}({})", def.name(), names.join(", "))
                })
                .collect::<Vec<_>>()
                .join("\n");
            self.output.push_back(super::Event::Output(OutputRequest {
                value: Value::String(listing),
                span: request.span.clone(),
                stream: OutputStream::Stdout,
            }));
        }
        self.complete_command(
            purpose,
            &Invocation::from_request(request),
            Ok(CommandResponse::success(Value::Absent)),
        );
        Ok(None)
    }

    pub(super) fn finish_definition(&mut self) -> Result<(), String> {
        let caller = self.callers.pop().ok_or("definition has no caller")?;
        let (purpose, returned, cleanup_failed) = self.restore_caller(caller);
        match purpose {
            CommandPurpose::Ordinary => {}
            CommandPurpose::Assertion {
                expected_ok,
                required,
                text,
                span,
            } => {
                self.assertion_result(cleanup_failed != expected_ok, required, text, &span);
            }
            CommandPurpose::Bind(dst) => {
                let mut response = CommandResponse::success(returned);
                response.exit_code = u8::from(cleanup_failed);
                self.put(dst, Slot::Result(response));
            }
            CommandPurpose::Thread { key, span } => {
                let mut response = CommandResponse::success(returned);
                response.exit_code = u8::from(cleanup_failed);
                self.finish_thread(key, &span, response);
            }
            CommandPurpose::Cleanup if cleanup_failed => self.cleanup_failed(),
            CommandPurpose::Cleanup => {}
        }
        Ok(())
    }

    pub(super) fn guard_site<'s>(&'s self, span: &'s Span) -> &'s Span {
        self.callers
            .iter()
            .rev()
            .take_while(|caller| !matches!(caller.purpose, CommandPurpose::Cleanup))
            .last()
            .map_or(span, |caller| &caller.invocation.span)
    }

    pub(super) fn call_context(&self) -> Option<(&str, &Position)> {
        let def = self.program.definitions().get(self.unit?)?;
        let call = self.callers.last()?;
        Some((def.name(), call.call_span.start()))
    }

    pub(super) fn unwind_failed_definition(
        &mut self,
        failure: ExecutionError,
    ) -> Result<(), ExecutionError> {
        let Some(caller) = self.callers.pop() else {
            return Err(failure);
        };
        let origin = Invocation {
            span: caller.invocation.span.clone(),
            display: caller.invocation.display.clone(),
        };
        let (purpose, _, _) = self.restore_caller(caller);
        match (purpose, failure) {
            (CommandPurpose::Cleanup, failure) => {
                let response = failure.guard.map_or_else(
                    || CommandResponse {
                        exit_code: 1,
                        stderr: failure.message,
                        ..CommandResponse::success(Value::Absent)
                    },
                    |guard| guard.response,
                );
                self.defer_failure(&origin, &response);
            }
            (
                purpose @ CommandPurpose::Assertion { .. },
                ExecutionError {
                    guard: Some(guard), ..
                },
            ) => {
                self.complete_command(purpose, &origin, Ok(guard.response));
            }
            (_, failure) => self.abort(failure),
        }
        Ok(())
    }

    pub(super) fn in_poll(&self) -> bool {
        !self.polls.is_empty() || self.callers.iter().any(|caller| !caller.polls.is_empty())
    }

    pub(super) fn propagate_retry(&mut self, retry: super::polls::RetrySignal) {
        let Some(caller) = self.callers.pop() else {
            self.error(&retry.span, "retry: outside any poll".to_owned());
            return;
        };
        let (purpose, _, _) = self.restore_caller(caller);
        if matches!(purpose, CommandPurpose::Cleanup) {
            self.cleanup_failed();
            return;
        }
        let base = self.polls.last().map_or_else(
            || self.callers.last().map_or(0, |caller| caller.deferred_base),
            |poll| poll.deferred_base,
        );
        // Defers in the caller belong to this attempt while retry unwinds.
        // A cleanup failure must abort before scheduling another attempt.
        self.attempt_cleanup = Some(retry.span.clone());
        self.pending_retry = Some(retry);
        self.draining = Some(base);
    }

    fn restore_caller(&mut self, caller: Caller) -> (CommandPurpose, Value, bool) {
        let returned = std::mem::replace(&mut self.returned, caller.returned);
        let cleanup_failed =
            std::mem::replace(&mut self.local_cleanup_failed, caller.cleanup_failed);
        self.unit = caller.unit;
        self.block = caller.block;
        self.offset = caller.offset;
        self.temps = caller.temps;
        self.frames.truncate(caller.frame_depth);
        self.loops = caller.loops;
        self.polls = caller.polls;
        self.attempt_cleanup = caller.attempt_cleanup;
        self.draining = caller.draining;
        self.failure = caller.prior_failure;
        self.pending_retry = caller.pending_retry;
        self.expression_observations = caller.observations;
        (caller.purpose, returned, cleanup_failed)
    }

    pub(super) fn cleanup_failed(&mut self) {
        self.exit_code = 1;
        if self.failure.is_none() {
            if let Some(span) = &self.attempt_cleanup {
                self.failure = Some(ExecutionError::new(
                    span,
                    "poll attempt deferred cleanup failed".to_owned(),
                ));
                self.draining = Some(self.callers.last().map_or(0, |caller| caller.deferred_base));
            }
        }
        if self.unit.is_some() {
            self.local_cleanup_failed = true;
        }
    }
}

fn parameter(argument: &Argument, annotation: Option<ParameterType>) -> Option<Value> {
    use ParameterType as T;
    match (argument, annotation) {
        (Argument::Word(text), Some(T::Number)) => Number::new(text.clone()).map(Value::Number),
        (Argument::Word(text), Some(T::Bool)) => match text.as_str() {
            "true" => Some(Value::Bool(true)),
            "false" => Some(Value::Bool(false)),
            _ => None,
        },
        (Argument::Word(text) | Argument::Quoted(text), None | Some(T::String)) => {
            Some(Value::String(text.clone()))
        }
        (Argument::Value(value), None)
        | (Argument::Value(value @ Value::String(_)), Some(T::String))
        | (Argument::Value(value @ Value::Number(_)), Some(T::Number))
        | (Argument::Value(value @ Value::Bool(_)), Some(T::Bool)) => Some(value.clone()),
        _ => None,
    }
}

fn parameter_type(annotation: ParameterType) -> &'static str {
    match annotation {
        ParameterType::Number => "number",
        ParameterType::String => "string",
        ParameterType::Bool => "bool",
    }
}

fn argument_kind(argument: &Argument) -> &'static str {
    match argument {
        Argument::Word(_) | Argument::Quoted(_) | Argument::Value(Value::String(_)) => "a string",
        Argument::Value(Value::Number(_)) => "a number",
        Argument::Value(Value::Bool(_)) => "a bool",
        Argument::Value(Value::Null) => "null",
        Argument::Value(Value::Absent) => "an absent value",
        Argument::Value(Value::List(_)) => "a list",
        Argument::Value(_) => "an object",
        Argument::Adapter { .. } => "an adapter",
    }
}
