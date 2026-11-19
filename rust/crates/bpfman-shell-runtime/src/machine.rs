use std::{collections::HashMap, fmt};

use bpfman_shell_ir::{self as ir, BlockId, Instruction, Program, Temp, Terminator};
use bpfman_shell_source::Span;

use crate::{Argument, Number, Value};

/// A suspended interpreter step or its final result.
pub enum Step<'a> {
    /// Resolve and perform a command invocation.
    Command {
        /// Invocation request.
        request: CommandRequest,
        /// State that accepts the command observation.
        continuation: CommandContinuation<'a>,
    },
    /// Report an expression statement's value.
    Output {
        /// Output request.
        request: OutputRequest,
        /// State that accepts the output observation.
        continuation: OutputContinuation<'a>,
    },
    /// The program has finished.
    Finished {
        /// Overall CLI exit status.
        exit_code: u8,
    },
    /// Execution stopped with a located error.
    Failed(ExecutionError),
}

/// How a command invocation is requested.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DispatchLane {
    /// An ordinary command.
    Command,
    /// A command whose outcome is consumed by a bind.
    Bind,
    /// A command previously registered by a defer.
    Deferred,
}

/// One invocation to perform.
#[derive(Debug, Eq, PartialEq)]
pub struct CommandRequest {
    /// Head argument evaluated before dispatch.
    pub head: Argument,
    /// Remaining arguments in source order.
    pub arguments: Vec<Argument>,
    /// Dispatch purpose.
    pub lane: DispatchLane,
    /// Invocation source extent.
    pub span: Span,
}

/// The driver's observation of an invocation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandResponse {
    /// Process or builtin exit status.
    pub exit_code: u8,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
    /// Primary value supplied by a typed builtin or definition.
    pub primary: Value,
    /// Whether job control requested termination.
    pub killed: bool,
    /// Signal name, when the command was terminated by a signal.
    pub signal: Option<String>,
    /// Process ID, when the adapter knows it.
    pub pid: Option<u32>,
}

/// An expression statement's output.
#[derive(Debug, Eq, PartialEq)]
pub struct OutputRequest {
    /// Value to render.
    pub value: Value,
    /// Statement source extent.
    pub span: Span,
}

/// The interpreter state suspended at command dispatch.
pub struct CommandContinuation<'a> {
    machine: Machine<'a>,
    purpose: CommandPurpose,
    span: Span,
}

impl<'a> CommandContinuation<'a> {
    /// Continue with a command result or transport failure.
    pub fn resume(mut self, observation: Result<CommandResponse, String>) -> Step<'a> {
        match observation {
            Ok(response) => match self.purpose {
                CommandPurpose::Ordinary => {
                    if response.exit_code != 0 {
                        let message = format!("command exited with status {}", response.exit_code);
                        self.machine.error(&self.span, message);
                    }
                }
                CommandPurpose::Bind(dst) => self.machine.put(dst, Slot::Result(response)),
                CommandPurpose::Cleanup => {
                    if response.exit_code != 0 {
                        self.machine.exit_code = 1;
                    }
                }
            },
            Err(message) => {
                if matches!(self.purpose, CommandPurpose::Cleanup) {
                    self.machine.exit_code = 1;
                } else {
                    self.machine.error(&self.span, message);
                }
            }
        }
        self.machine.advance()
    }
}

/// The interpreter state suspended while reporting an expression result.
pub struct OutputContinuation<'a> {
    machine: Machine<'a>,
    span: Span,
}

impl<'a> OutputContinuation<'a> {
    /// Continue after the driver accepts the output or reports a failure.
    pub fn resume(mut self, observation: Result<(), String>) -> Step<'a> {
        if let Err(message) = observation {
            self.machine.error(&self.span, message);
        }
        self.machine.advance()
    }
}

/// One failure with the source extent of the failing operation.
#[derive(Debug, Eq, PartialEq)]
pub struct ExecutionError {
    /// Extent of the failing operation.
    pub span: Span,
    /// Explanation without location formatting.
    pub message: String,
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let start = self.span.start();
        write!(
            f,
            "{}:{}:{}: {}",
            start.name().as_str(),
            start.line(),
            start.column(),
            self.message
        )
    }
}

impl std::error::Error for ExecutionError {}

/// Start the verified program using an explicit initial environment.
pub fn execute(program: &Program, bindings: HashMap<String, Value>) -> Step<'_> {
    if let Some(definition) = program.definitions().first() {
        return Step::Failed(ExecutionError {
            span: definition.span().clone(),
            message: "definition execution is not yet implemented".to_owned(),
        });
    }
    Machine {
        program,
        block: program.entry(),
        offset: 0,
        temps: (0..program.num_temps()).map(|_| None).collect(),
        frames: vec![bindings],
        deferred: Vec::new(),
        draining: false,
        failure: None,
        exit_code: 0,
    }
    .advance()
}

#[derive(Clone)]
struct Argv {
    head: Argument,
    arguments: Vec<Argument>,
}

impl Argv {
    fn request(self, lane: DispatchLane, span: Span) -> CommandRequest {
        CommandRequest {
            head: self.head,
            arguments: self.arguments,
            lane,
            span,
        }
    }
}

enum Slot {
    Value(Value),
    Args(Argv),
    Result(CommandResponse),
}

enum CommandPurpose {
    Ordinary,
    Bind(Temp),
    Cleanup,
}

struct Machine<'a> {
    program: &'a Program,
    block: BlockId,
    offset: usize,
    temps: Vec<Option<Slot>>,
    frames: Vec<HashMap<String, Value>>,
    deferred: Vec<(Argv, Span)>,
    draining: bool,
    failure: Option<ExecutionError>,
    exit_code: u8,
}

enum Action {
    Next,
    Command(CommandRequest, CommandPurpose),
    Output(OutputRequest),
    Stop,
}

impl<'a> Machine<'a> {
    fn advance(mut self) -> Step<'a> {
        loop {
            if self.draining {
                if let Some((argv, span)) = self.deferred.pop() {
                    return self.suspend_command(
                        argv.request(DispatchLane::Deferred, span),
                        CommandPurpose::Cleanup,
                    );
                }
                self.draining = false;
                if let Some(failure) = self.failure.take() {
                    return Step::Failed(failure);
                }
            }

            let block = &self.program.blocks()[self.block.0];
            let (action, span) = if let Some(instruction) = block.instructions.get(self.offset) {
                self.offset += 1;
                (self.instruction(instruction), &instruction.span)
            } else {
                (
                    self.terminate(&block.terminator.node),
                    &block.terminator.span,
                )
            };
            match action {
                Ok(Action::Next) => {}
                Ok(Action::Stop) => {
                    return Step::Finished {
                        exit_code: self.exit_code,
                    };
                }
                Ok(Action::Command(request, purpose)) => {
                    return self.suspend_command(request, purpose);
                }
                Ok(Action::Output(request)) => {
                    let span = request.span.clone();
                    return Step::Output {
                        request,
                        continuation: OutputContinuation {
                            machine: self,
                            span,
                        },
                    };
                }
                Err(message) => self.error(span, message),
            }
        }
    }

    fn error(&mut self, span: &Span, message: String) {
        self.failure = Some(ExecutionError {
            span: span.clone(),
            message,
        });
        self.draining = true;
    }

    fn suspend_command(self, request: CommandRequest, purpose: CommandPurpose) -> Step<'a> {
        let span = request.span.clone();
        Step::Command {
            request,
            continuation: CommandContinuation {
                machine: self,
                purpose,
                span,
            },
        }
    }

    fn put(&mut self, temp: Temp, slot: Slot) {
        self.temps[temp.0] = Some(slot);
    }

    fn value(&self, temp: Temp) -> Result<&Value, String> {
        match self.temps.get(temp.0).and_then(Option::as_ref) {
            Some(Slot::Value(value)) => Ok(value),
            _ => Err(format!("expected value at t{}", temp.0)),
        }
    }

    fn argv(&self, temp: Temp) -> Result<Argv, String> {
        match self.temps.get(temp.0).and_then(Option::as_ref) {
            Some(Slot::Args(argv)) => Ok(argv.clone()),
            _ => Err(format!("expected argv at t{}", temp.0)),
        }
    }

    fn bind(&mut self, name: &str, value: Value) -> Result<(), String> {
        if name != "_" {
            let frame = self.frames.last_mut().ok_or("missing lexical frame")?;
            frame.insert(name.to_owned(), value);
        }
        Ok(())
    }

    fn instruction(&mut self, instruction: &ir::Located<Instruction>) -> Result<Action, String> {
        let span = &instruction.span;
        match &instruction.node {
            Instruction::EnterProgramDefers | Instruction::TraceBranch(_) => {}
            Instruction::EnterIfFrame => self.frames.push(HashMap::new()),
            Instruction::ExitFrame => {
                if self.frames.len() <= 1 {
                    return Err("cannot leave the program frame".to_owned());
                }
                self.frames.pop();
            }
            Instruction::RunProgramDefers => self.draining = true,
            Instruction::Eval { dst, expression } => {
                self.put(*dst, Slot::Value(self.eval(expression)?));
            }
            Instruction::BindName { name, src } => self.bind(name, self.value(*src)?.clone())?,
            Instruction::EmitResult { src } => {
                return Ok(Action::Output(OutputRequest {
                    value: self.value(*src)?.clone(),
                    span: span.clone(),
                }));
            }
            Instruction::BuildArgs { dst, command } => {
                let argv = Argv {
                    head: self.argument(&command.head)?,
                    arguments: command
                        .arguments
                        .iter()
                        .map(|value| self.argument(value))
                        .collect::<Result<_, _>>()?,
                };
                self.put(*dst, Slot::Args(argv));
            }
            Instruction::DispatchCommand { argv } => {
                let request = self
                    .argv(*argv)?
                    .request(DispatchLane::Command, span.clone());
                return Ok(Action::Command(request, CommandPurpose::Ordinary));
            }
            Instruction::DispatchBind {
                dst,
                argv,
                call_span,
                ..
            } => {
                let request = self
                    .argv(*argv)?
                    .request(DispatchLane::Bind, call_span.clone());
                return Ok(Action::Command(request, CommandPurpose::Bind(*dst)));
            }
            Instruction::ApplyBind {
                src,
                target,
                policy,
                ..
            } => {
                let Some(Slot::Result(response)) = self.temps.get(src.0).and_then(Option::as_ref)
                else {
                    return Err(format!("expected command result at t{}", src.0));
                };
                match policy {
                    ir::BindPolicy::Capture => {
                        self.bind(target, Value::Outcome(Box::new(response.clone())))?
                    }
                    ir::BindPolicy::Guard { .. } if response.exit_code == 0 => {
                        let value = if response.primary == Value::Absent {
                            Value::Outcome(Box::new(response.clone()))
                        } else {
                            response.primary.clone()
                        };
                        self.bind(target, value)?;
                    }
                    ir::BindPolicy::Guard { on_failure } if response.exit_code != 0 => {
                        self.failure = Some(ExecutionError {
                            span: span.clone(),
                            message: format!(
                                "guard {target}: command failed (exit {})",
                                response.exit_code
                            ),
                        });
                        self.jump(*on_failure);
                    }
                    ir::BindPolicy::Guard { .. } => return Err("invalid guard outcome".to_owned()),
                }
            }
            Instruction::RegisterDefer { argv } => {
                self.deferred.push((self.argv(*argv)?, span.clone()))
            }
            _ => return Err("instruction is not yet implemented".to_owned()),
        }
        Ok(Action::Next)
    }

    fn jump(&mut self, block: BlockId) {
        self.block = block;
        self.offset = 0;
    }

    fn terminate(&mut self, terminator: &Terminator) -> Result<Action, String> {
        match *terminator {
            Terminator::Stop => return Ok(Action::Stop),
            Terminator::Jump(target) => self.jump(target),
            Terminator::Branch {
                condition,
                on_true,
                on_false,
            } => {
                let Value::Bool(value) = self.value(condition)? else {
                    return Err("condition must evaluate to a boolean".to_owned());
                };
                self.jump(if *value { on_true } else { on_false });
            }
            Terminator::PropagateError => return Err("missing active error".to_owned()),
            _ => return Err("terminator is not yet implemented".to_owned()),
        }
        Ok(Action::Next)
    }

    fn argument(&self, expression: &ir::Expression) -> Result<Argument, String> {
        Ok(match &expression.node {
            ir::ExpressionKind::Word(text) => Argument::Word(text.clone()),
            ir::ExpressionKind::Quoted(text) => Argument::Quoted(text.clone()),
            _ => Argument::Value(self.eval(expression)?),
        })
    }

    fn eval(&self, expression: &ir::Expression) -> Result<Value, String> {
        Ok(match &expression.node {
            ir::ExpressionKind::Word(word) => match word.as_str() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                "null" => Value::Null,
                text => match Number::new(text.to_owned()) {
                    Some(number) => Value::Number(number),
                    None => {
                        let bytes = text.as_bytes();
                        let index = usize::from(matches!(bytes.first(), Some(b'-' | b'+')));
                        if bytes.get(index).is_some_and(u8::is_ascii_digit) {
                            return Err(format!("invalid numeric literal {text:?}"));
                        }
                        Value::String(text.to_owned())
                    }
                },
            },
            ir::ExpressionKind::Quoted(text) => Value::String(text.clone()),
            ir::ExpressionKind::Variable(variable) => {
                let mut value = self
                    .frames
                    .iter()
                    .rev()
                    .find_map(|frame| frame.get(&variable.name))
                    .cloned()
                    .ok_or_else(|| format!("undefined variable: {}", variable.name))?;
                for field in variable.path.split('.').filter(|field| !field.is_empty()) {
                    let Value::Outcome(outcome) = value else {
                        return Err(format!("cannot access field {field:?}"));
                    };
                    value = outcome_field(&outcome, field)?;
                }
                value
            }
            ir::ExpressionKind::List(items) => Value::List(
                items
                    .iter()
                    .map(|item| self.eval(item))
                    .collect::<Result<_, _>>()?,
            ),
            _ => return Err("expression is not yet implemented".to_owned()),
        })
    }
}

fn outcome_field(outcome: &CommandResponse, field: &str) -> Result<Value, String> {
    match field {
        "ok" => Ok(Value::Bool(outcome.exit_code == 0)),
        "exit_code" => Number::new(outcome.exit_code.to_string())
            .map(Value::Number)
            .ok_or_else(|| "invalid exit status".to_owned()),
        "stdout" => Ok(Value::String(outcome.stdout.clone())),
        "stderr" => Ok(Value::String(outcome.stderr.clone())),
        "killed" => Ok(Value::Bool(outcome.killed)),
        "signal" => Ok(Value::String(outcome.signal.clone().unwrap_or_default())),
        "pid" => Ok(outcome
            .pid
            .and_then(|pid| Number::new(pid.to_string()))
            .map_or(Value::Absent, Value::Number)),
        "value" => Ok(outcome.primary.clone()),
        _ => Err(format!("unknown outcome field {field:?}")),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::Path};

    use bpfman_shell_check::{check, expand_imports, prepare_imports};
    use bpfman_shell_lower::lower;
    use bpfman_shell_syntax::parse;

    use super::*;

    fn program(source: &str) -> Program {
        let parsed =
            parse("runtime.bpfman", source).unwrap_or_else(|error| unreachable!("{error}"));
        let plan =
            prepare_imports(parsed, Path::new(".")).unwrap_or_else(|error| unreachable!("{error}"));
        let expanded =
            expand_imports(plan, &BTreeMap::new()).unwrap_or_else(|error| unreachable!("{error}"));
        let checked = check(expanded).unwrap_or_else(|issues| unreachable!("{issues:?}"));
        lower(&checked).unwrap_or_else(|error| unreachable!("{error}"))
    }

    fn result(exit_code: u8, primary: Value) -> Result<CommandResponse, String> {
        Ok(CommandResponse {
            exit_code,
            primary,
            stdout: String::new(),
            stderr: String::new(),
            killed: false,
            signal: None,
            pid: None,
        })
    }

    #[test]
    fn responses_drive_typed_steps_and_deferred_argv_is_captured_in_lifo_order() {
        let program = program(
            "let n = 99999999999999999999999999999999\ndefer echo $n\nlet n = null\ndefer echo $n\nlet r <- bpfman program get 1\n($r.value)",
        );
        let Step::Command {
            request,
            continuation,
        } = execute(&program, HashMap::new())
        else {
            unreachable!("expected captured command")
        };
        assert_eq!(request.lane, DispatchLane::Bind);
        assert_eq!(
            request.arguments,
            ["program", "get", "1"].map(|word| Argument::Word(word.to_owned()))
        );
        let Step::Output {
            request,
            continuation,
        } = continuation.resume(result(0, Value::Null))
        else {
            unreachable!("expected expression output")
        };
        assert_eq!(request.value, Value::Null);
        let Step::Command {
            request,
            continuation,
        } = continuation.resume(Ok(()))
        else {
            unreachable!("expected first defer")
        };
        assert_eq!(request.lane, DispatchLane::Deferred);
        assert_eq!(request.arguments, [Argument::Value(Value::Null)]);
        let Step::Command {
            request,
            continuation,
        } = continuation.resume(result(0, Value::Absent))
        else {
            unreachable!("expected second defer")
        };
        let [Argument::Value(Value::Number(number))] = request.arguments.as_slice() else {
            unreachable!("expected the original exact number")
        };
        assert_eq!(number.as_str(), "99999999999999999999999999999999");
        assert!(matches!(
            continuation.resume(result(0, Value::Absent)),
            Step::Finished { exit_code: 0 }
        ));
    }

    #[test]
    fn guard_failure_drains_deferred_work_before_reporting_original_error() {
        let program =
            program("defer echo cleanup\nif true { guard _ <- exec false }\nprint unreachable");
        let Step::Command { continuation, .. } = execute(&program, HashMap::new()) else {
            unreachable!("expected guard command")
        };
        let Step::Command {
            request,
            continuation,
        } = continuation.resume(result(7, Value::Absent))
        else {
            unreachable!("expected defer")
        };
        assert_eq!(request.lane, DispatchLane::Deferred);
        let Step::Failed(error) = continuation.resume(Err("cleanup transport failure".to_owned()))
        else {
            unreachable!("expected original guard failure")
        };
        assert_eq!(error.span.start().line(), 2);
        assert!(error.message.contains("exit 7"));
    }
}
