use bpfman_shell_ir::{BlockId, ParameterType};

use super::{
    Argument, CommandPurpose, CommandRequest, CommandResponse, ExecutionError, Machine, Number,
    Slot, Value, loops,
};

const MAX_DEPTH: usize = 256;

pub(super) struct Caller {
    unit: Option<usize>,
    block: BlockId,
    offset: usize,
    temps: Vec<Option<Slot>>,
    pub(super) frame_depth: usize,
    loops: Vec<loops::Iteration>,
    pub(super) deferred_base: usize,
    draining: Option<usize>,
    returned: Value,
    cleanup_failed: bool,
    prior_failure: Option<ExecutionError>,
    purpose: CommandPurpose,
}

impl Machine<'_> {
    pub(super) fn start_definition(
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
            return Ok(Some(purpose));
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
                format!(
                    "def {head}: invalid argument for parameter {:?}",
                    param.name
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
            deferred_base: self.deferred.len(),
            draining: self.draining.take(),
            returned: std::mem::replace(&mut self.returned, Value::Absent),
            cleanup_failed: std::mem::take(&mut self.local_cleanup_failed),
            prior_failure: self.failure.take(),
            purpose,
        });
        self.jump(BlockId(0));
        Ok(None)
    }

    pub(super) fn finish_definition(&mut self) -> Result<(), String> {
        let caller = self.callers.pop().ok_or("definition has no caller")?;
        let (purpose, returned, cleanup_failed) = self.restore_caller(caller);
        match purpose {
            CommandPurpose::Ordinary => {}
            CommandPurpose::Bind(dst) => self.put(
                dst,
                Slot::Result(CommandResponse {
                    exit_code: u8::from(cleanup_failed),
                    stdout: String::new(),
                    stderr: String::new(),
                    primary: returned,
                    killed: false,
                    signal: None,
                    pid: None,
                }),
            ),
            CommandPurpose::Cleanup if cleanup_failed => self.cleanup_failed(),
            CommandPurpose::Cleanup => {}
        }
        Ok(())
    }

    pub(super) fn unwind_failed_definition(
        &mut self,
        failure: ExecutionError,
    ) -> Result<(), ExecutionError> {
        let Some(caller) = self.callers.pop() else {
            return Err(failure);
        };
        let (purpose, _, _) = self.restore_caller(caller);
        if matches!(purpose, CommandPurpose::Cleanup) {
            self.cleanup_failed();
        } else {
            self.error(&failure.span, failure.message);
        }
        Ok(())
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
        self.draining = caller.draining;
        self.failure = caller.prior_failure;
        (caller.purpose, returned, cleanup_failed)
    }

    pub(super) fn cleanup_failed(&mut self) {
        self.exit_code = 1;
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
