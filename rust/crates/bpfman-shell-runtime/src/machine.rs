use std::{collections::HashMap, fmt};

use bpfman_shell_ir::{self as ir, BlockId, Instruction, Program, Temp, Terminator};
use bpfman_shell_source::Span;

use crate::{Argument, Number, Value};

mod calls;
mod compare;
mod loops;
mod matches;
mod polls;
mod pure;

pub use polls::{ClockContinuation, PauseContinuation};

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
    /// Request the driver's current monotonic time.
    ReadClock(ClockContinuation<'a>),
    /// Request an interruptible delay.
    Pause {
        /// Requested delay.
        duration: std::time::Duration,
        /// State that accepts completion of the delay.
        continuation: PauseContinuation<'a>,
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
    /// A command whose status is inspected by an assertion.
    Assertion,
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
    /// Destination selected by the runtime.
    pub stream: OutputStream,
}

/// Output channel for a rendered value or diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputStream {
    /// Ordinary script output.
    Stdout,
    /// Assertion diagnostic.
    Stderr,
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
                CommandPurpose::Assertion {
                    expected_ok,
                    required,
                    text,
                    span,
                } => {
                    self.machine.assertion_result(
                        (response.exit_code == 0) == expected_ok,
                        required,
                        text,
                        &span,
                    );
                }
                CommandPurpose::Cleanup => {
                    if response.exit_code != 0 {
                        self.machine.cleanup_failed();
                    }
                }
            },
            Err(message) => {
                if matches!(self.purpose, CommandPurpose::Cleanup) {
                    self.machine.cleanup_failed();
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
    Machine {
        program,
        block: program.entry(),
        offset: 0,
        temps: (0..program.num_temps()).map(|_| None).collect(),
        frames: vec![bindings],
        loops: Vec::new(),
        polls: Vec::new(),
        attempt_cleanup: None,
        diagnostic: None,
        deferred: Vec::new(),
        draining: None,
        unit: None,
        callers: Vec::new(),
        returned: Value::Absent,
        local_cleanup_failed: false,
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
    Assertion {
        expected_ok: bool,
        required: bool,
        text: String,
        span: Span,
    },
}

struct Machine<'a> {
    program: &'a Program,
    block: BlockId,
    offset: usize,
    temps: Vec<Option<Slot>>,
    frames: Vec<HashMap<String, Value>>,
    loops: Vec<loops::Iteration>,
    polls: Vec<polls::Poll>,
    attempt_cleanup: Option<Span>,
    diagnostic: Option<OutputRequest>,
    deferred: Vec<(Argv, Span)>,
    draining: Option<usize>,
    unit: Option<usize>,
    callers: Vec<calls::Caller>,
    returned: Value,
    local_cleanup_failed: bool,
    failure: Option<ExecutionError>,
    exit_code: u8,
}

enum Action {
    Next,
    Command(CommandRequest, CommandPurpose),
    Output(OutputRequest),
    Clock(polls::ClockPurpose),
    Stop,
}

impl<'a> Machine<'a> {
    fn advance(mut self) -> Step<'a> {
        loop {
            if let Some(request) = self.diagnostic.take() {
                let span = request.span.clone();
                return Step::Output {
                    request,
                    continuation: OutputContinuation {
                        machine: self,
                        span,
                    },
                };
            }
            if let Some(base) = self.draining {
                if self.deferred.len() > base {
                    if let Some((argv, span)) = self.deferred.pop() {
                        let request = argv.request(DispatchLane::Deferred, span);
                        match self.start_definition(&request, CommandPurpose::Cleanup) {
                            Ok(Some(purpose)) => return self.suspend_command(request, purpose),
                            Ok(None) => {}
                            Err(_) => self.cleanup_failed(),
                        }
                        continue;
                    }
                }
                self.draining = None;
                self.attempt_cleanup = None;
                if let Some(failure) = self.failure.take() {
                    if let Err(failure) = self.unwind_failed_definition(failure) {
                        return Step::Failed(failure);
                    }
                    continue;
                }
            }

            let blocks = self.unit.map_or_else(
                || self.program.blocks(),
                |index| self.program.definitions()[index].blocks(),
            );
            let block = &blocks[self.block.0];
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
                Ok(Action::Clock(purpose)) => {
                    return Step::ReadClock(ClockContinuation {
                        machine: self,
                        purpose,
                        span: span.clone(),
                    });
                }
                Ok(Action::Command(request, purpose)) => {
                    match self.start_definition(&request, purpose) {
                        Ok(Some(purpose)) => return self.suspend_command(request, purpose),
                        Ok(None) => {}
                        Err(message) => self.error(&request.span, message),
                    }
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
        self.draining = Some(self.callers.last().map_or(0, |caller| caller.deferred_base));
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
            Instruction::EnterProgramDefers
            | Instruction::TraceBranch(_)
            | Instruction::TraceLoop(_) => {}
            Instruction::EnterIfFrame
            | Instruction::EnterDefinitionFrame
            | Instruction::EnterAttemptFrame => self.frames.push(HashMap::new()),
            Instruction::EnterDefinitionDefers
            | Instruction::EnterAttemptDefers
            | Instruction::TracePoll(_) => {}
            Instruction::ExitFrame => {
                if self.frames.len() <= 1 {
                    return Err("cannot leave the program frame".to_owned());
                }
                self.frames.pop();
            }
            Instruction::RunProgramDefers => self.draining = Some(0),
            Instruction::RunAttemptDefers => {
                self.draining = Some(self.polls.last().map_or_else(
                    || self.callers.last().map_or(0, |caller| caller.deferred_base),
                    |poll| poll.deferred_base,
                ));
                self.attempt_cleanup = Some(span.clone());
            }
            Instruction::RunDefinitionDefers => {
                self.draining = self.callers.last().map(|caller| caller.deferred_base);
            }
            Instruction::Eval { dst, expression } => {
                self.put(*dst, Slot::Value(self.eval(expression)?));
            }
            Instruction::BindName { name, src } => self.bind(name, self.value(*src)?.clone())?,
            Instruction::BindDestructure { bindings, src } => {
                let value = self.value(*src)?.clone();
                self.destructure(bindings.names(), value)?;
            }
            Instruction::EmitResult { src } => {
                return Ok(Action::Output(OutputRequest {
                    value: self.value(*src)?.clone(),
                    span: span.clone(),
                    stream: OutputStream::Stdout,
                }));
            }
            Instruction::Assert { required, clause } => match clause.as_ref() {
                ir::Assertion::Expression(expression) => {
                    let truth = boolean(self.eval(expression)?)?;
                    self.assertion_result(truth, *required, expression.to_string(), span);
                }
                ir::Assertion::Command {
                    expected,
                    command,
                    negated,
                    ..
                } => {
                    let argv = self.build_argv(command)?;
                    let expected_ok = (*expected == ir::CommandStatus::Ok) != *negated;
                    let text = format!(
                        "expected command to {}",
                        if expected_ok { "succeed" } else { "fail" }
                    );
                    let request = argv.request(DispatchLane::Assertion, span.clone());
                    return Ok(Action::Command(
                        request,
                        CommandPurpose::Assertion {
                            expected_ok,
                            required: *required,
                            text,
                            span: span.clone(),
                        },
                    ));
                }
            },
            Instruction::BuildArgs { dst, command } => {
                let argv = self.build_argv(command)?;
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
        }
        Ok(Action::Next)
    }

    fn build_argv(&self, command: &ir::Command) -> Result<Argv, String> {
        Ok(Argv {
            head: self.argument(&command.head)?,
            arguments: command
                .arguments
                .iter()
                .map(|value| self.argument(value))
                .collect::<Result<_, _>>()?,
        })
    }

    fn assertion_result(&mut self, truth: bool, required: bool, text: String, span: &Span) {
        if truth {
            return;
        }
        if required {
            self.error(span, format!("require failed: {text}"));
        } else {
            self.exit_code = 1;
            let position = span.start();
            self.diagnostic = Some(OutputRequest {
                value: Value::String(format!(
                    "{}:{}:{}: assertion failed: {text}",
                    position.name().as_str(),
                    position.line(),
                    position.column()
                )),
                span: span.clone(),
                stream: OutputStream::Stderr,
            });
        }
    }

    fn jump(&mut self, block: BlockId) {
        self.block = block;
        self.offset = 0;
    }

    fn terminate(&mut self, terminator: &Terminator) -> Result<Action, String> {
        match *terminator {
            Terminator::Stop => return Ok(Action::Stop),
            Terminator::Jump(target) => {
                if self.polls.last().is_some_and(|poll| poll.success == target) {
                    self.polls.pop();
                }
                self.jump(target);
            }
            Terminator::BeginPoll {
                schedule,
                attempt,
                timeout,
                success,
            } => {
                return Ok(Action::Clock(polls::ClockPurpose::Start {
                    schedule,
                    attempt,
                    timeout,
                    success,
                }));
            }
            Terminator::RetryPoll { target, message } => return self.retry_poll(target, message),
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
            Terminator::ReturnValue { src, epilogue } => {
                self.returned = self.value(src)?.clone();
                let caller = self
                    .callers
                    .last()
                    .ok_or("return has no definition caller")?;
                self.frames.truncate(caller.frame_depth + 1);
                self.loops.clear();
                self.jump(epilogue);
            }
            Terminator::FinishDefinition => self.finish_definition()?,
            Terminator::PropagateError => {
                return Err(self
                    .poll_timeout()
                    .unwrap_or_else(|| "missing active error".to_owned()));
            }
            Terminator::ForEach {
                list,
                ref bindings,
                body,
                exit,
            } => {
                self.start_loop(list, bindings, body, exit, None)?;
            }
            Terminator::ForEachCollect {
                list,
                ref bindings,
                ref target,
                mode,
                body,
                exit,
            } => {
                self.start_loop(
                    list,
                    bindings,
                    body,
                    exit,
                    Some(loops::Collection::new(target.clone(), mode)),
                )?;
            }
            Terminator::ForEachContinue(owner) => self.advance_loop(owner, false)?,
            Terminator::ExitLoop(owner) => self.advance_loop(owner, true)?,
            Terminator::CollectProduce { owner, result, .. } => {
                self.collect_result(owner, result)?
            }
            Terminator::Unreachable => return Err("entered an unreachable block".to_owned()),
        }
        Ok(Action::Next)
    }

    fn argument(&self, expression: &ir::Expression) -> Result<Argument, String> {
        Ok(match &expression.node {
            ir::ExpressionKind::Word(text) => Argument::Word(text.clone()),
            ir::ExpressionKind::Quoted(text) => Argument::Quoted(text.clone()),
            ir::ExpressionKind::Variable(variable) => Argument::Value(self.lookup(variable, true)?),
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
            ir::ExpressionKind::Variable(variable) => self.lookup(variable, false)?,
            ir::ExpressionKind::Binary {
                left,
                operator,
                right,
            } => compare::evaluate(*operator, self.eval(left)?, self.eval(right)?)?,
            ir::ExpressionKind::Logical {
                left,
                operator,
                right,
            } => {
                let left = boolean(self.eval(left)?)?;
                let short = matches!(
                    (operator, left),
                    (ir::LogicalOperator::And, false) | (ir::LogicalOperator::Or, true)
                );
                Value::Bool(if short {
                    left
                } else {
                    boolean(self.eval(right)?)?
                })
            }
            ir::ExpressionKind::Unary { operator, operand } => {
                let value = self.eval(operand)?;
                match operator {
                    ir::UnaryOperator::Not => Value::Bool(!boolean(value)?),
                    ir::UnaryOperator::NotEmpty => Value::Bool(!is_empty(&value)),
                    ir::UnaryOperator::Negate => {
                        let Value::Number(number) = value else {
                            return Err("negation requires a number".to_owned());
                        };
                        let text = number.as_str();
                        let negated = if let Some(positive) = text.strip_prefix('-') {
                            positive.to_owned()
                        } else {
                            format!("-{text}")
                        };
                        Value::Number(Number::new(negated).ok_or("invalid negated number")?)
                    }
                }
            }
            ir::ExpressionKind::Interpolated(parts) => {
                let mut text = String::new();
                for part in parts {
                    match part {
                        ir::InterpolatedPart::Literal(literal) => text.push_str(literal),
                        ir::InterpolatedPart::Expression(value) => {
                            let value = self.eval(value)?;
                            text.push_str(
                                &value
                                    .scalar_text()
                                    .ok_or("interpolation requires a scalar")?,
                            );
                        }
                    }
                }
                Value::String(text)
            }
            ir::ExpressionKind::List(items) => Value::List(
                items
                    .iter()
                    .map(|item| self.eval(item))
                    .collect::<Result<_, _>>()?,
            ),
            ir::ExpressionKind::Matches { target, block } => {
                Value::Bool(self.matches(self.eval(target)?, block)?)
            }
            ir::ExpressionKind::PureCall { builtin, arguments } => {
                self.pure_call(*builtin, arguments)?
            }
            ir::ExpressionKind::Record(fields) => Value::Record(
                fields
                    .iter()
                    .map(|field| {
                        self.eval(&field.value)
                            .map(|value| (field.name.clone(), value))
                    })
                    .collect::<Result<_, _>>()?,
            ),
            _ => return Err("expression is not yet implemented".to_owned()),
        })
    }
    fn lookup(&self, variable: &ir::VariableRef, soft: bool) -> Result<Value, String> {
        let value = self
            .frames
            .iter()
            .rev()
            .find_map(|frame| frame.get(&variable.name))
            .cloned()
            .ok_or_else(|| format!("undefined variable: {}", variable.name))?;
        let value = self.lookup_path(value, &variable.path)?;
        if value == Value::Absent && !soft && !variable.path.is_empty() {
            Err(format!(
                "{}: path {:?} is absent",
                variable.name, variable.path
            ))
        } else {
            Ok(value)
        }
    }

    fn lookup_path(&self, mut value: Value, mut path: &str) -> Result<Value, String> {
        let find = |name: &str| self.frames.iter().rev().find_map(|frame| frame.get(name));
        while !path.is_empty() {
            path = path.strip_prefix('.').unwrap_or(path);
            if let Some(index_path) = path.strip_prefix('[') {
                let (index, rest) = index_path.split_once(']').ok_or("unclosed index")?;
                let index = if let Some(variable) = index.strip_prefix('$') {
                    find(variable)
                        .and_then(Value::scalar_text)
                        .and_then(|text| text.parse::<usize>().ok())
                } else {
                    index.parse::<usize>().ok()
                }
                .ok_or("invalid list index")?;
                let Value::List(items) = value else {
                    return Err("cannot index a non-list".to_owned());
                };
                value = items.into_iter().nth(index).unwrap_or(Value::Absent);
                path = rest;
            } else {
                let boundary = path.find(['.', '[']).unwrap_or(path.len());
                let (field, rest) = path.split_at(boundary);
                value = field_value(value, field)?;
                path = rest;
            }
            if value == Value::Absent {
                return Ok(Value::Absent);
            }
        }
        Ok(value)
    }
}

fn boolean(value: Value) -> Result<bool, String> {
    if let Value::Bool(value) = value {
        Ok(value)
    } else {
        Err("condition must evaluate to a boolean".to_owned())
    }
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Absent | Value::Null | Value::Bool(false) => true,
        Value::String(text) => text.is_empty(),
        Value::Number(number) => number
            .as_str()
            .parse::<f64>()
            .is_ok_and(|value| value == 0.0),
        Value::List(values) => values.is_empty(),
        Value::Record(fields) | Value::BpfmanRecord { fields, .. } => fields.is_empty(),
        _ => false,
    }
}

fn field_value(value: Value, field: &str) -> Result<Value, String> {
    match value {
        Value::Outcome(outcome) => outcome_field(&outcome, field),
        Value::Record(mut fields) | Value::BpfmanRecord { mut fields, .. } => {
            Ok(fields.remove(field).unwrap_or(Value::Absent))
        }
        _ => Err(format!("cannot access field {field:?}")),
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
        _ => Ok(Value::Absent),
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

    #[test]
    fn collecting_loop_keeps_partial_results_and_unwinds_iteration_frames() {
        let program = program(
            "let item = \"outer\"\nlet batch <- foreach (stop item) in [[false 1] [false 2] [true 3]] { if $stop { break }\n echo $item }\nprint $item $batch.ok $batch.values",
        );
        let Step::Command {
            request,
            continuation,
        } = execute(&program, HashMap::new())
        else {
            unreachable!("expected first producer");
        };
        assert_eq!(request.lane, DispatchLane::Bind);
        assert_eq!(
            request.arguments,
            [Argument::Value(Value::Number(
                Number::new("1".to_owned()).unwrap_or_else(|| unreachable!())
            ))]
        );
        let Step::Command {
            request,
            continuation,
        } = continuation.resume(result(1, Value::Absent))
        else {
            unreachable!("expected second producer");
        };
        assert_eq!(
            request.arguments,
            [Argument::Value(Value::Number(
                Number::new("2".to_owned()).unwrap_or_else(|| unreachable!())
            ))]
        );
        let Step::Command {
            request,
            continuation,
        } = continuation.resume(result(0, Value::Null))
        else {
            unreachable!("expected output command");
        };
        assert_eq!(request.lane, DispatchLane::Command);
        assert_eq!(
            request.arguments,
            [
                Argument::Value(Value::String("outer".to_owned())),
                Argument::Value(Value::Bool(false)),
                Argument::Value(Value::List(vec![Value::Null])),
            ]
        );
        assert!(matches!(
            continuation.resume(result(0, Value::Absent)),
            Step::Finished { exit_code: 0 }
        ));
    }

    #[test]
    fn hoisted_definition_returns_to_its_caller_after_local_defers() {
        let program = program(
            "defer echo global\nguard n <- helper 7\nprint $n\ndef helper(arg: number) { defer echo local\n foreach n in [1] { return $arg } }",
        );
        let Step::Command {
            request,
            continuation,
        } = execute(&program, HashMap::new())
        else {
            unreachable!("expected definition-local defer");
        };
        assert_eq!(request.lane, DispatchLane::Deferred);
        assert_eq!(request.arguments, [Argument::Word("local".to_owned())]);
        let Step::Command {
            request,
            continuation,
        } = continuation.resume(result(0, Value::Absent))
        else {
            unreachable!("expected caller command");
        };
        assert_eq!(request.head, Argument::Word("print".to_owned()));
        let [Argument::Value(Value::Number(number))] = request.arguments.as_slice() else {
            unreachable!("expected numeric return");
        };
        assert_eq!(number.as_str(), "7");
        let Step::Command {
            request,
            continuation,
        } = continuation.resume(result(0, Value::Absent))
        else {
            unreachable!("expected program defer");
        };
        assert_eq!(request.arguments, [Argument::Word("global".to_owned())]);
        assert!(matches!(
            continuation.resume(result(0, Value::Absent)),
            Step::Finished { exit_code: 0 }
        ));
    }

    #[test]
    fn failed_deferred_definition_preserves_the_original_error_and_other_defers() {
        let program = program(
            "def cleanup() { guard _ <- exec false }\ndefer echo last\ndefer cleanup\nguard _ <- exec false",
        );
        let Step::Command { continuation, .. } = execute(&program, HashMap::new()) else {
            unreachable!("expected main guard");
        };
        let Step::Command { continuation, .. } = continuation.resume(result(7, Value::Absent))
        else {
            unreachable!("expected deferred definition guard");
        };
        let Step::Command {
            request,
            continuation,
        } = continuation.resume(result(9, Value::Absent))
        else {
            unreachable!("expected remaining program defer");
        };
        assert_eq!(request.arguments, [Argument::Word("last".to_owned())]);
        let Step::Failed(error) = continuation.resume(result(0, Value::Absent)) else {
            unreachable!("expected failure");
        };
        assert!(error.message.contains("exit 7"));
        assert_eq!(error.span.start().line(), 4);
    }

    #[test]
    fn polling_requests_time_and_only_retries_after_attempt_cleanup() {
        use std::time::Duration;

        let program =
            program("poll timeout 5s every 1s { defer echo cleanup\n retry \"waiting\" }");
        let Step::ReadClock(clock) = execute(&program, HashMap::new()) else {
            unreachable!("expected initial clock observation");
        };
        let Step::Command {
            request,
            continuation,
        } = clock.resume(Ok(Duration::ZERO))
        else {
            unreachable!("expected first attempt cleanup");
        };
        assert_eq!(request.lane, DispatchLane::Deferred);
        let Step::ReadClock(clock) = continuation.resume(result(0, Value::Absent)) else {
            unreachable!("expected retry clock observation");
        };
        let Step::Pause {
            duration,
            continuation,
        } = clock.resume(Ok(Duration::from_secs(1)))
        else {
            unreachable!("expected a delay between attempts");
        };
        assert_eq!(duration, Duration::from_secs(1));
        let Step::Command { continuation, .. } = continuation.resume(Ok(())) else {
            unreachable!("expected second attempt cleanup");
        };
        let Step::ReadClock(clock) = continuation.resume(result(0, Value::Absent)) else {
            unreachable!("expected second retry clock observation");
        };
        let Step::Failed(error) = clock.resume(Ok(Duration::from_secs(5))) else {
            unreachable!("expected timeout at the deadline");
        };
        assert_eq!(error.message, "poll timed out after 2 attempt(s): waiting");
    }

    #[test]
    fn evaluation_preserves_large_integer_order_and_short_circuits() {
        let program = program(
            "let high = 99999999999999999999999999999999\nlet low = 99999999999999999999999999999998\nlet index = 1\nlet data = record { items: [spare ready] }\nif $high > $low or jq . $low { print \"${data.items[$index]} ${$high}\" }",
        );
        let Step::Command {
            request,
            continuation,
        } = execute(&program, HashMap::new())
        else {
            unreachable!("expected a command after the short-circuit");
        };
        assert_eq!(
            request.arguments,
            [Argument::Value(Value::String(
                "ready 99999999999999999999999999999999".to_owned()
            ))]
        );
        assert!(matches!(
            continuation.resume(result(0, Value::Absent)),
            Step::Finished { exit_code: 0 }
        ));
    }

    #[test]
    fn deterministic_builtins_preserve_typed_lists_and_presence() {
        let program = program(
            "let r = record { field: null }\nlet xs = range 2\nlet pairs = zip $xs [one two]\nlet bytes = u32le \"0x12\"\nlet n = null $r.field\nlet m = missing r.other\nprint $pairs[1][1] $bytes $n $m",
        );
        let Step::Command { request, .. } = execute(&program, HashMap::new()) else {
            unreachable!("expected builtin results");
        };
        assert_eq!(
            request.arguments,
            [
                Argument::Value(Value::String("two".to_owned())),
                Argument::Value(Value::String("12000000".to_owned())),
                Argument::Value(Value::Bool(true)),
                Argument::Value(Value::Bool(true)),
            ]
        );
    }
}
