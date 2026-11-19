use std::{
    collections::HashMap,
    io::{self, Write},
    process::Command,
    time::Instant,
};

use bpfman_shell_ir::Program;
use bpfman_shell_runtime::{
    Argument, CommandRequest, CommandResponse, DispatchLane, OutputStream, QueryRequest, Step,
    TracePart, Value, execute_with_trace,
};

mod bpfman;
mod files;
mod jobs;
mod kernel;
mod kfunc;
mod lsm;
mod net;
mod process;
mod procfs;
mod query;
mod registry;
mod slot_pool;
mod uprobe;

pub(super) use registry::Registry as ImageRegistry;

#[derive(Default)]
struct Fixtures {
    functions: kfunc::Pool,
    network: net::Network,
    probes: lsm::Probes,
}

pub(super) fn run(
    program: &Program,
    stdout: &mut impl Write,
    registry: &mut ImageRegistry,
    trace: bool,
) -> Result<u8, String> {
    let start = Instant::now();
    let interrupt = process::Interrupt::register().map_err(|error| error.to_string())?;
    let mut jobs = jobs::Jobs::default();
    let mut fixtures = Fixtures::default();
    let mut step = execute_with_trace(program, HashMap::new(), trace);
    loop {
        step = match step {
            Step::Command {
                request,
                continuation,
            } => continuation.resume(dispatch(
                request,
                stdout,
                &interrupt,
                &mut jobs,
                &mut fixtures,
                registry,
            )),
            Step::Output {
                request,
                continuation,
            } => continuation.resume(
                interrupt
                    .check()
                    .and_then(|()| files::render(&request.value, true))
                    .and_then(|value| {
                        match request.stream {
                            OutputStream::Stdout => writeln!(stdout, "{value}"),
                            OutputStream::Stderr => writeln!(io::stderr().lock(), "{value}"),
                        }
                        .map_err(|error| error.to_string())
                    }),
            ),
            Step::Trace {
                request,
                continuation,
            } => {
                let position = request.span.start();
                let mut line = format!("+ {}:{}: ", position.name().as_str(), position.line());
                for part in request.parts {
                    line.push_str(&match part {
                        TracePart::Text(text) => text,
                        TracePart::Argument(argument) => {
                            files::render_argument(&argument, false).unwrap_or_default()
                        }
                        TracePart::Value(value) => files::render(&value, false)
                            .unwrap_or_else(|_| "<unrenderable value>".to_owned()),
                    });
                }
                let _ = writeln!(io::stderr().lock(), "{line}");
                continuation.resume(Ok(()))
            }
            Step::PathExists {
                request,
                continuation,
            } => {
                let present = interrupt.check().and_then(|()| {
                    std::path::Path::new(&request.path)
                        .try_exists()
                        .map_err(|error| error.to_string())
                });
                continuation.resume(present)
            }
            Step::Query {
                request,
                continuation,
            } => continuation.resume(interrupt.check().and_then(|()| query::run(request))),
            Step::ReadClock(continuation) => {
                continuation.resume(interrupt.check().map(|()| start.elapsed()))
            }
            Step::Pause {
                duration,
                continuation,
            } => continuation.resume(interrupt.pause(duration)),
            Step::Finished { exit_code } => return jobs.finish(Ok(exit_code)),
            Step::Failed(error) => {
                return jobs.finish(if error.was_reported() {
                    Ok(1)
                } else {
                    Err(error.to_string())
                });
            }
        };
    }
}

fn dispatch(
    request: CommandRequest,
    stdout: &mut impl Write,
    interrupt: &process::Interrupt,
    jobs: &mut jobs::Jobs,
    fixtures: &mut Fixtures,
    registry: &mut ImageRegistry,
) -> Result<CommandResponse, String> {
    interrupt.check()?;
    let head = argument(&request.head)?;
    if head == "bpfman" {
        return bpfman::invoke(request.arguments, interrupt, registry);
    }
    if head == "start" {
        return jobs.start(&request.arguments, cite(&request));
    }
    if head == "fire" {
        let words = request
            .arguments
            .iter()
            .map(argument)
            .collect::<Result<Vec<_>, _>>()?;
        let (kind, waves) = crate::fixturemode::Waves::builtin(&words)?;
        let executable = std::env::current_exe().map_err(|error| format!("fire: {error}"))?;
        let program = executable
            .into_os_string()
            .into_string()
            .map_err(|_| "fire: shell path must be UTF-8")?;
        let argv = std::iter::once(program)
            .chain(waves.argv())
            .map(Argument::Word)
            .collect::<Vec<_>>();
        return jobs.start_fixture(&argv, cite(&request), kind.mode(), kind.publishes_binary());
    }
    if matches!(head.as_str(), "uprobe" | "uprobe-target") {
        return uprobe::invoke(&head, &request.arguments, interrupt);
    }
    if head == "kfunc" {
        return fixtures
            .functions
            .invoke(&request.arguments, cite(&request), interrupt);
    }
    if head == "net" {
        return fixtures
            .network
            .invoke(&request.arguments, cite(&request), interrupt, jobs);
    }
    if head == "lsm" {
        return fixtures.probes.invoke(&request.arguments, interrupt);
    }
    if head == "linkinfo" {
        return Ok(kernel::linkinfo(&request.arguments));
    }
    if head == "proginfo" {
        return Ok(kernel::proginfo(&request.arguments));
    }
    if head == "registry" {
        return Ok(builtin_response(registry.invoke(&request.arguments)));
    }
    if head == "process" {
        return Ok(builtin_response(procfs::invoke(&request.arguments)));
    }
    if head == "wait" {
        return jobs.wait(&request.arguments, interrupt);
    }
    if head == "kill" {
        return jobs.kill(&request.arguments, interrupt);
    }
    if matches!(head.as_str(), "jobs" | "reap") && !request.arguments.is_empty() {
        return Err(format!("{head} takes no arguments"));
    }
    if head == "jobs" {
        return jobs.list((request.lane == DispatchLane::Command).then_some(stdout));
    }
    if head == "reap" {
        return jobs.reap();
    }
    if head == "jq" {
        let [filter, input] = <[Argument; 2]>::try_from(request.arguments)
            .map_err(|_| "usage: jq <filter> <value>".to_owned())?;
        let mut values = query::run(QueryRequest {
            filter: argument(&filter)?,
            input: input.into(),
            span: request.span,
        })?;
        let primary = match values.len() {
            0 => Value::Absent,
            1 => values.pop().unwrap_or(Value::Absent),
            _ => Value::List(values),
        };
        return Ok(CommandResponse {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            primary,
            killed: false,
            signal: None,
            pid: None,
        });
    }
    if head == "print" {
        let pretty = request.arguments.len() == 1;
        let line = request
            .arguments
            .iter()
            .map(|argument| files::render_argument(argument, pretty))
            .collect::<Result<Vec<_>, _>>()?
            .join(" ");
        if matches!(request.lane, DispatchLane::Command | DispatchLane::Deferred) {
            writeln!(stdout, "{line}").map_err(|error| error.to_string())?;
        }
        return Ok(CommandResponse {
            exit_code: 0,
            stdout: format!("{line}\n"),
            stderr: String::new(),
            primary: Value::Absent,
            killed: false,
            signal: None,
            pid: None,
        });
    }
    if head == "tempdir" || head == "file" {
        let primary = if head == "tempdir" {
            files::tempdir(&request.arguments)?
        } else {
            files::tempfile(&request.arguments)?
        };
        if head == "file" && request.lane == DispatchLane::Command {
            writeln!(stdout, "{}", scalar(&primary)?).map_err(|error| error.to_string())?;
        }
        return Ok(CommandResponse {
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            primary,
            killed: false,
            signal: None,
            pid: None,
        });
    }
    let (executable, arguments) = if head == "exec" {
        let Some((executable, remaining)) = request.arguments.split_first() else {
            return Err("exec: expected a command".to_owned());
        };
        (argument(executable)?, remaining)
    } else {
        (head, request.arguments.as_slice())
    };
    let mut files = Vec::new();
    let arguments = arguments
        .iter()
        .map(|value| files::external_argument(value, &mut files))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            if process::has_executable(&executable) {
                error
            } else {
                format!("{executable}: command not found")
            }
        })?;
    let output = process::capture(Command::new(&executable).args(arguments), interrupt)?;
    if request.lane == DispatchLane::Command
        || (request.lane == DispatchLane::Deferred && output.status.success())
    {
        stdout
            .write_all(&output.stdout)
            .map_err(|error| error.to_string())?;
        io::stderr()
            .lock()
            .write_all(&output.stderr)
            .map_err(|error| error.to_string())?;
    }
    Ok(process::response(output))
}

fn argument(argument: &Argument) -> Result<String, String> {
    match argument {
        Argument::Word(text) | Argument::Quoted(text) => Ok(text.clone()),
        Argument::Value(value) | Argument::Adapter { value, .. } => scalar(value),
    }
}

fn builtin_response(result: Result<Value, String>) -> CommandResponse {
    match result {
        Ok(value) => CommandResponse::success(value),
        Err(stderr) => CommandResponse {
            exit_code: 1,
            stderr,
            ..CommandResponse::success(Value::Absent)
        },
    }
}

fn cite(request: &CommandRequest) -> String {
    let position = request.statement_span.start();
    format!(
        "{}:{}:{}",
        position.name().as_str(),
        position.line(),
        position.column()
    )
}

fn scalar(value: &Value) -> Result<String, String> {
    value
        .scalar_text()
        .ok_or_else(|| "expected a scalar".to_owned())
}
