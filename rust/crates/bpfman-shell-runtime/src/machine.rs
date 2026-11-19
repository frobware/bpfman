use std::{collections::HashMap, fmt};

use bpfman_shell_ir::{self as ir, BlockId, Instruction, Program, Temp, Terminator};
use bpfman_shell_source::Span;

use crate::{Argument, Number, Value};

mod assertions;
mod calls;
mod compare;
mod loops;
mod matches;
mod observations;
mod polls;
mod pure;
mod trace;
pub use trace::{TracePart, TraceRequest};

use observations::{EvalError, EvalResult};
pub use observations::{
    PathContinuation, PathRequest, QueryContinuation, QueryInput, QueryRequest,
};

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
    /// Report a trace event with already evaluated values.
    Trace {
        /// Trace event and its source extent.
        request: TraceRequest,
        /// Continue after handling the event.
        continuation: OutputContinuation<'a>,
    },
    /// Evaluate a jq query in the driver's query adapter.
    Query {
        /// Filter and input.
        request: QueryRequest,
        /// State that accepts a stream of query results.
        continuation: QueryContinuation<'a>,
    },
    /// Request a filesystem path observation.
    PathExists {
        /// Path to observe.
        request: PathRequest,
        /// State that accepts the filesystem observation.
        continuation: PathContinuation<'a>,
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
    /// A command whose primary value is consumed by a thread expression.
    Thread,
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

impl CommandResponse {
    /// A successful builtin result with a primary value and no captured output.
    #[must_use]
    pub fn success(primary: Value) -> Self {
        Self {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            primary,
            killed: false,
            signal: None,
            pid: None,
        }
    }
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
        self.machine
            .complete_command(self.purpose, &self.span, observation);
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
    execute_with_trace(program, bindings, false)
}

/// Start the verified program with its initial tracing setting.
pub fn execute_with_trace(
    program: &Program,
    bindings: HashMap<String, Value>,
    trace_enabled: bool,
) -> Step<'_> {
    let mut machine = Machine {
        program,
        block: program.entry(),
        offset: 0,
        temps: (0..program.num_temps()).map(|_| None).collect(),
        frames: vec![bindings],
        loops: Vec::new(),
        polls: Vec::new(),
        attempt_cleanup: None,
        output: Default::default(),
        trace_enabled,
        pending_effect: None,
        pending_retry: None,
        expression_observations: HashMap::new(),
        deferred: Vec::new(),
        draining: None,
        unit: None,
        callers: Vec::new(),
        returned: Value::Absent,
        local_cleanup_failed: false,
        failure: None,
        exit_code: 0,
        assert_failures: 0,
    };
    machine.trace_definitions();
    machine.advance()
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
    Thread {
        key: *const ir::Expression,
        span: Span,
    },
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
    output: std::collections::VecDeque<Event>,
    trace_enabled: bool,
    pending_effect: Option<PendingEffect>,
    pending_retry: Option<polls::RetrySignal>,
    expression_observations: HashMap<*const ir::Expression, Value>,
    deferred: Vec<(Argv, Span)>,
    draining: Option<usize>,
    unit: Option<usize>,
    callers: Vec<calls::Caller>,
    returned: Value,
    local_cleanup_failed: bool,
    failure: Option<ExecutionError>,
    exit_code: u8,
    assert_failures: usize,
}

enum Event {
    Output(OutputRequest),
    Trace(TraceRequest),
}

enum PendingEffect {
    Command(CommandRequest, CommandPurpose),
    Clock(polls::ClockPurpose, Span),
}

enum Action {
    Next,
    Command(Box<CommandRequest>, CommandPurpose),
    Clock(polls::ClockPurpose),
    Stop,
}

impl<'a> Machine<'a> {
    fn advance(mut self) -> Step<'a> {
        loop {
            if let Some(event) = self.output.pop_front() {
                return match event {
                    Event::Output(request) => {
                        let continuation = OutputContinuation {
                            span: request.span.clone(),
                            machine: self,
                        };
                        Step::Output {
                            request,
                            continuation,
                        }
                    }
                    Event::Trace(request) => {
                        let continuation = OutputContinuation {
                            span: request.span.clone(),
                            machine: self,
                        };
                        Step::Trace {
                            request,
                            continuation,
                        }
                    }
                };
            }
            if let Some(effect) = self.pending_effect.take() {
                return match effect {
                    PendingEffect::Command(request, purpose) => {
                        self.suspend_command(request, purpose)
                    }
                    PendingEffect::Clock(purpose, span) => Step::ReadClock(ClockContinuation {
                        machine: self,
                        purpose,
                        span,
                    }),
                };
            }
            if let Some(base) = self.draining {
                if self.deferred.len() > base {
                    if let Some((argv, span)) = self.deferred.pop() {
                        self.trace_argv(&span, "defer fire: ", &argv);
                        let request = argv.request(DispatchLane::Deferred, span);
                        match self.start_call(&request, CommandPurpose::Cleanup) {
                            Ok(Some(purpose)) => {
                                self.pending_effect = Some(PendingEffect::Command(request, purpose))
                            }
                            Ok(None) => {}
                            Err(_) => self.cleanup_failed(),
                        }
                        continue;
                    }
                }
                self.draining = None;
                self.attempt_cleanup = None;
                if let Some(failure) = self.failure.take() {
                    self.pending_retry = None;
                    if let Err(failure) = self.unwind_failed_definition(failure) {
                        return Step::Failed(failure);
                    }
                    continue;
                }
                if let Some(retry) = self.pending_retry.take() {
                    if let Some(owner) = self.polls.last().map(|poll| poll.owner) {
                        match self.prepare_retry(owner, retry.message) {
                            Ok(()) => {
                                self.pending_effect = Some(PendingEffect::Clock(
                                    polls::ClockPurpose::Retry,
                                    retry.span,
                                ));
                            }
                            Err(message) => self.error(&retry.span, message),
                        }
                    } else {
                        self.propagate_retry(retry);
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
                let action = self.instruction(instruction);
                if !matches!(
                    &action,
                    Err(EvalError::Path { .. }
                        | EvalError::Query { .. }
                        | EvalError::Thread { .. })
                ) {
                    self.expression_observations.clear();
                }
                (action, &instruction.span)
            } else {
                (
                    self.terminate(&block.terminator).map_err(EvalError::from),
                    &block.terminator.span,
                )
            };
            match action {
                Ok(Action::Next) => {}
                Ok(Action::Stop) => {
                    let offenses = std::mem::take(&mut self.assert_failures);
                    if offenses > 0 {
                        self.report(span, format!("{offenses} assertion(s) failed"));
                    } else {
                        return Step::Finished {
                            exit_code: self.exit_code,
                        };
                    }
                }
                Ok(Action::Clock(purpose)) => {
                    self.pending_effect = Some(PendingEffect::Clock(purpose, span.clone()));
                }
                Ok(Action::Command(request, purpose)) => match self.start_call(&request, purpose) {
                    Ok(Some(purpose)) => {
                        self.pending_effect = Some(PendingEffect::Command(*request, purpose))
                    }
                    Ok(None) => {}
                    Err(message) => self.error(&request.span, message),
                },
                Err(EvalError::Message(message)) => self.error(span, message),
                Err(EvalError::Path { key, request }) => {
                    self.offset -= 1;
                    let span = request.span.clone();
                    return Step::PathExists {
                        request,
                        continuation: PathContinuation {
                            machine: self,
                            key,
                            span,
                        },
                    };
                }
                Err(EvalError::Query { key, request, mode }) => {
                    self.offset -= 1;
                    let span = request.span.clone();
                    return Step::Query {
                        request: *request,
                        continuation: QueryContinuation {
                            machine: self,
                            key,
                            span,
                            mode,
                        },
                    };
                }
                Err(EvalError::Thread { key, request }) => {
                    self.offset -= 1;
                    let purpose = CommandPurpose::Thread {
                        key,
                        span: request.span.clone(),
                    };
                    match self.start_call(&request, purpose) {
                        Ok(Some(purpose)) => {
                            self.pending_effect = Some(PendingEffect::Command(*request, purpose))
                        }
                        Ok(None) => {}
                        Err(message) => self.error(&request.span, message),
                    }
                }
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

    fn complete_command(
        &mut self,
        purpose: CommandPurpose,
        span: &Span,
        observation: Result<CommandResponse, String>,
    ) {
        match observation {
            Ok(response) => match purpose {
                CommandPurpose::Ordinary => {
                    if response.exit_code != 0 {
                        let message = format!("command exited with status {}", response.exit_code);
                        self.error(span, message);
                    }
                }
                CommandPurpose::Bind(dst) => self.put(dst, Slot::Result(response)),
                CommandPurpose::Thread { key, span } => {
                    self.finish_thread(key, &span, response);
                }
                CommandPurpose::Assertion {
                    expected_ok,
                    required,
                    text,
                    span,
                } => {
                    self.assertion_result(
                        (response.exit_code == 0) == expected_ok,
                        required,
                        text,
                        &span,
                    );
                }
                CommandPurpose::Cleanup => {
                    if response.exit_code != 0 {
                        self.cleanup_failed();
                    }
                }
            },
            Err(message) => {
                if matches!(purpose, CommandPurpose::Cleanup) {
                    self.cleanup_failed();
                } else {
                    self.error(span, message);
                }
            }
        }
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

    fn instruction(&mut self, instruction: &ir::Located<Instruction>) -> EvalResult<Action> {
        let span = &instruction.span;
        match &instruction.node {
            Instruction::EnterProgramDefers => {}
            Instruction::TraceBranch(branch) => self.trace_note(
                span,
                match branch {
                    ir::BranchTrace::Then => "if then",
                    ir::BranchTrace::Elif => "if elif",
                    ir::BranchTrace::Else => "if else",
                    ir::BranchTrace::Skip => "if skip",
                },
            ),
            Instruction::TraceLoop(event) => self.trace_note(
                span,
                match event {
                    ir::LoopTrace::Break => "break",
                    ir::LoopTrace::Continue => "continue",
                },
            ),
            Instruction::TracePoll(event) => self.trace_note(
                span,
                match event {
                    ir::PollTrace::Attempt => "poll attempt",
                    ir::PollTrace::Timeout => "poll timeout",
                },
            ),
            Instruction::EnterIfFrame
            | Instruction::EnterDefinitionFrame
            | Instruction::EnterAttemptFrame => self.frames.push(HashMap::new()),
            Instruction::EnterDefinitionDefers | Instruction::EnterAttemptDefers => {}
            Instruction::ExitFrame => {
                if self.frames.len() <= 1 {
                    return Err("cannot leave the program frame".into());
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
            Instruction::BindName { name, src, trace } => {
                let value = self.value(*src)?.clone();
                if *trace && self.trace_enabled {
                    self.trace_value(span, format!("let {name} = "), &value);
                }
                self.bind(name, value)?;
            }
            Instruction::BindDestructure { bindings, src } => {
                let value = self.value(*src)?.clone();
                self.trace_bindings(span, "let", bindings.names(), &value);
                self.destructure(bindings.names(), value)?;
            }
            Instruction::EmitResult { src } => {
                let value = self.value(*src)?.clone();
                self.trace_value(span, "expr ".to_owned(), &value);
                self.output.push_back(Event::Output(OutputRequest {
                    value,
                    span: span.clone(),
                    stream: OutputStream::Stdout,
                }));
            }
            Instruction::Assert { required, clause } => {
                match clause.as_ref() {
                    ir::Assertion::Expression(expression) => {
                        if !required && self.in_poll() {
                            return Err("assert is not valid inside poll; use retry unless ... or require ...".into());
                        }
                        self.assert_expression(expression, *required, span)?;
                    }
                    ir::Assertion::Command {
                        expected,
                        command,
                        negated,
                        ..
                    } => {
                        if !required && self.in_poll() {
                            return Err("assert is not valid inside poll; use retry unless ... or require ...".into());
                        }
                        let argv = self.build_argv(command)?;
                        let expected_ok = (*expected == ir::CommandStatus::Ok) != *negated;
                        let text = format!(
                            "expected command to {}",
                            if expected_ok { "succeed" } else { "fail" }
                        );
                        let request = argv.request(DispatchLane::Assertion, span.clone());
                        return Ok(Action::Command(
                            Box::new(request),
                            CommandPurpose::Assertion {
                                expected_ok,
                                required: *required,
                                text,
                                span: span.clone(),
                            },
                        ));
                    }
                }
            }
            Instruction::BuildArgs { dst, command } => {
                let argv = self.build_argv(command)?;
                self.put(*dst, Slot::Args(argv));
            }
            Instruction::DispatchCommand { argv } => {
                let argv = self.argv(*argv)?;
                self.trace_argv(span, "", &argv);
                let request = argv.request(DispatchLane::Command, span.clone());
                return Ok(Action::Command(Box::new(request), CommandPurpose::Ordinary));
            }
            Instruction::DispatchBind {
                dst,
                argv,
                call_span,
                trace_header,
            } => {
                let argv = self.argv(*argv)?;
                if self.trace_enabled {
                    self.trace_argv(span, &format!("{trace_header} <- "), &argv);
                }
                let request = argv.request(DispatchLane::Bind, call_span.clone());
                return Ok(Action::Command(
                    Box::new(request),
                    CommandPurpose::Bind(*dst),
                ));
            }
            Instruction::ApplyBind {
                src,
                target,
                policy,
                ..
            } => {
                let Some(Slot::Result(response)) = self.temps.get(src.0).and_then(Option::as_ref)
                else {
                    return Err(format!("expected command result at t{}", src.0).into());
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
                    ir::BindPolicy::Guard { .. } => return Err("invalid guard outcome".into()),
                }
            }
            Instruction::RegisterDefer { argv } => {
                let argv = self.argv(*argv)?;
                self.trace_argv(span, "defer ", &argv);
                self.deferred.push((argv, span.clone()));
            }
        }
        Ok(Action::Next)
    }

    fn build_argv(&self, command: &ir::Command) -> EvalResult<Argv> {
        Ok(Argv {
            head: self.argument(&command.head)?,
            arguments: command
                .arguments
                .iter()
                .map(|value| self.argument(value))
                .collect::<Result<_, _>>()?,
        })
    }

    fn eval_thread(
        &self,
        expression: &ir::Expression,
        input: &ir::Expression,
        command: &ir::Command,
    ) -> EvalResult<Value> {
        let key = std::ptr::from_ref(expression);
        if let Some(value) = self.expression_observations.get(&key) {
            return Ok(value.clone());
        }
        let mut argv = self.build_argv(command)?;
        argv.arguments.push(self.argument(input)?);
        Err(EvalError::Thread {
            key,
            request: Box::new(argv.request(DispatchLane::Thread, expression.span.clone())),
        })
    }

    fn finish_thread(
        &mut self,
        key: *const ir::Expression,
        span: &Span,
        response: CommandResponse,
    ) {
        if response.exit_code != 0 {
            let mut message = format!("thread: command failed (exit {})", response.exit_code);
            if !response.stderr.is_empty() {
                message.push_str(": ");
                message.push_str(&response.stderr);
            }
            self.expression_observations.clear();
            self.error(span, message);
            return;
        }
        let value = if response.primary == Value::Absent {
            Value::Outcome(Box::new(response))
        } else {
            response.primary
        };
        self.expression_observations.insert(key, value);
    }

    fn report(&mut self, span: &Span, message: String) {
        self.output.push_back(Event::Output(OutputRequest {
            value: Value::String(message),
            span: span.clone(),
            stream: OutputStream::Stderr,
        }));
    }

    fn jump(&mut self, block: BlockId) {
        self.block = block;
        self.offset = 0;
    }

    fn terminate(&mut self, terminator: &ir::Located<Terminator>) -> Result<Action, String> {
        match terminator.node {
            Terminator::Stop => return Ok(Action::Stop),
            Terminator::Jump(target) => {
                if self.polls.last().is_some_and(|poll| poll.success == target) {
                    if let Some(poll) = self.polls.pop() {
                        self.trace_note(&poll.span, "poll success");
                    }
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
            Terminator::RetryPoll { target, message } => {
                return self.retry_poll(target, message, &terminator.span);
            }
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
                let value = self.value(src)?.clone();
                self.trace_value(&terminator.span, "return ".to_owned(), &value);
                self.returned = value;
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
                self.start_loop(list, bindings, body, exit, None, &terminator.span)?;
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
                    &terminator.span,
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

    fn argument(&self, expression: &ir::Expression) -> EvalResult<Argument> {
        Ok(match &expression.node {
            ir::ExpressionKind::Word(text) => Argument::Word(text.clone()),
            ir::ExpressionKind::Quoted(text) => Argument::Quoted(text.clone()),
            ir::ExpressionKind::Variable(variable) => Argument::Value(self.lookup(variable, true)?),
            ir::ExpressionKind::Adapter { name, variable } => {
                let value = self.lookup(variable, false)?;
                if matches!(value, Value::Null | Value::Absent) {
                    return Err(
                        format!("adapter {name}: variable {} is null", variable.name).into(),
                    );
                }
                Argument::Adapter {
                    name: name.clone(),
                    value,
                }
            }
            _ => Argument::Value(self.eval(expression)?),
        })
    }

    fn eval(&self, expression: &ir::Expression) -> EvalResult<Value> {
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
                            return Err(format!("invalid numeric literal {text:?}").into());
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
                            return Err("negation requires a number".into());
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
                Value::Bool(self.matches(self.eval(target)?, block)?.passed())
            }
            ir::ExpressionKind::PureCall {
                builtin: ir::PureBuiltin::PathExists,
                arguments,
            } => self.path_exists(expression, arguments)?,
            ir::ExpressionKind::PureCall {
                builtin: ir::PureBuiltin::Jq,
                arguments,
            } => {
                let [filter, input] = arguments.as_slice() else {
                    return Err("jq requires a filter and input".into());
                };
                self.query(
                    expression,
                    self.argument(filter)?,
                    self.argument(input)?,
                    observations::QueryMode::Expression,
                )?
            }
            ir::ExpressionKind::Thread { value, command, .. }
                if matches!(&command.head.node, ir::ExpressionKind::Word(name) if name == "jq")
                    && !self
                        .program
                        .definitions()
                        .iter()
                        .any(|definition| definition.name() == "jq") =>
            {
                let [filter] = command.arguments.as_slice() else {
                    return Err("threaded jq requires a filter".into());
                };
                self.query(
                    expression,
                    self.argument(filter)?,
                    self.argument(value)?,
                    observations::QueryMode::Thread,
                )?
            }
            ir::ExpressionKind::Thread { value, command, .. } => {
                self.eval_thread(expression, value, command)?
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
            _ => return Err("expression is not yet implemented".into()),
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

fn location(span: &Span) -> String {
    let pos = span.start();
    format!("{}:{}:{}", pos.name().as_str(), pos.line(), pos.column())
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
        value => value
            .record_fields()
            .map(|fields| fields.get(field).cloned().unwrap_or(Value::Absent))
            .ok_or_else(|| format!("cannot access field {field:?}")),
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

    #[test]
    fn helper_retry_unwinds_all_definition_and_attempt_defers_before_clock() {
        use std::time::Duration;

        let program = program(
            "def inner() { defer echo inner\n retry \"waiting\" }\ndef outer() { defer echo outer\n inner }\npoll timeout 1s every 1ms { defer echo attempt\n outer }",
        );
        let Step::ReadClock(clock) = execute(&program, HashMap::new()) else {
            unreachable!("expected initial clock");
        };
        let mut step = clock.resume(Ok(Duration::ZERO));
        for argument in ["inner", "outer", "attempt"] {
            let Step::Command {
                request,
                continuation,
            } = step
            else {
                unreachable!("expected {argument} cleanup");
            };
            assert_eq!(request.lane, DispatchLane::Deferred);
            assert_eq!(request.arguments, [Argument::Word(argument.to_owned())]);
            step = continuation.resume(result(0, Value::Absent));
        }
        let Step::ReadClock(clock) = step else {
            unreachable!("expected retry clock");
        };
        let Step::Failed(error) = clock.resume(Ok(Duration::from_secs(1))) else {
            unreachable!("expected timeout");
        };
        assert!(error.message.ends_with(": waiting"));
    }

    #[test]
    fn two_queries_in_one_expression_resume_in_order_without_reissuing_the_first() {
        let program = program("if (path-exists first) and (path-exists second) { print present }");
        let Step::PathExists {
            request,
            continuation,
        } = execute(&program, HashMap::new())
        else {
            unreachable!("expected first query");
        };
        assert_eq!(request.path, "first");
        let Step::PathExists {
            request,
            continuation,
        } = continuation.resume(Ok(true))
        else {
            unreachable!("expected second query");
        };
        assert_eq!(request.path, "second");
        let Step::Command { continuation, .. } = continuation.resume(Ok(true)) else {
            unreachable!("expected branch action");
        };
        assert!(matches!(
            continuation.resume(result(0, Value::Absent)),
            Step::Finished { exit_code: 0 }
        ));
    }

    #[test]
    fn query_effect_preserves_input_origin_and_result_cardinality() {
        let program = program("let result = 'null' |> jq . |> jq .\n($result)");
        let Step::Query {
            request,
            continuation,
        } = execute(&program, HashMap::new())
        else {
            unreachable!("expected literal query");
        };
        assert_eq!(request.filter, ".");
        assert!(matches!(request.input, QueryInput::JsonText(text) if text == "null"));
        let Step::Query {
            request,
            continuation,
        } = continuation.resume(Ok(vec![Value::Null]))
        else {
            unreachable!("expected threaded query");
        };
        assert!(matches!(request.input, QueryInput::Value(Value::Null)));
        let Step::Output {
            request,
            continuation,
        } = continuation.resume(Ok(vec![Value::Null, Value::Bool(true)]))
        else {
            unreachable!("expected materialized results");
        };
        assert_eq!(
            request.value,
            Value::List(vec![Value::Null, Value::Bool(true)])
        );
        assert!(matches!(
            continuation.resume(Ok(())),
            Step::Finished { exit_code: 0 }
        ));
    }

    #[test]
    fn query_call_keeps_a_bound_string_as_a_value() {
        let program = program("let input = '1]'\nlet result = (jq . $input)");
        let Step::Query { request, .. } = execute(&program, HashMap::new()) else {
            unreachable!("expected query");
        };
        assert!(matches!(request.input, QueryInput::Value(Value::String(text)) if text == "1]"));
    }
}
