use std::{
    collections::HashMap,
    io::{self, Write},
    process::Command,
    time::Instant,
};

use bpfman_shell_ir::Program;
use bpfman_shell_runtime::{
    Argument, CommandRequest, CommandResponse, DispatchLane, OutputStream, QueryRequest, Step,
    Value, execute,
};

mod bpfman;
mod files;
mod jobs;
mod process;
mod query;

pub(super) fn run(program: &Program, stdout: &mut impl Write) -> Result<u8, String> {
    let start = Instant::now();
    let interrupt = process::Interrupt::register().map_err(|error| error.to_string())?;
    let mut jobs = jobs::Jobs::default();
    let mut step = execute(program, HashMap::new());
    loop {
        step = match step {
            Step::Command {
                request,
                continuation,
            } => continuation.resume(dispatch(request, stdout, &interrupt, &mut jobs)),
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
            Step::Failed(error) => return jobs.finish(Err(error.to_string())),
        };
    }
}

fn dispatch(
    request: CommandRequest,
    stdout: &mut impl Write,
    interrupt: &process::Interrupt,
    jobs: &mut jobs::Jobs,
) -> Result<CommandResponse, String> {
    interrupt.check()?;
    let head = argument(&request.head)?;
    if head == "bpfman" {
        return bpfman::invoke(request.arguments, interrupt);
    }
    if head == "start" {
        let position = request.span.start();
        let origin = format!(
            "{}:{}:{}",
            position.name().as_str(),
            position.line(),
            position.column()
        );
        return jobs.start(&request.arguments, origin);
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
        if request.lane == DispatchLane::Command {
            writeln!(stdout, "{line}").map_err(|error| error.to_string())?;
        }
        return Ok(CommandResponse {
            exit_code: 0,
            stdout: line,
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
    let mut files = Vec::new();
    let arguments = request
        .arguments
        .iter()
        .map(|value| files::external_argument(value, &mut files))
        .collect::<Result<Vec<_>, _>>()?;
    let (executable, arguments) = if head == "exec" {
        let Some((executable, remaining)) = arguments.split_first() else {
            return Err("exec: expected a command".to_owned());
        };
        (executable.as_str(), remaining)
    } else {
        (head.as_str(), arguments.as_slice())
    };
    // The Rust driver currently supplies only print and subprocess effects.
    // Refuse known typed builtins until their adapters are implemented.
    if head != "exec"
        && matches!(
            head.as_str(),
            "defs"
                | "fire"
                | "import"
                | "kfunc"
                | "linkinfo"
                | "lsm"
                | "net"
                | "process"
                | "proginfo"
                | "range"
                | "registry"
                | "trace"
                | "u32le"
                | "u64le"
                | "uprobe"
                | "uprobe-target"
                | "zip"
        )
    {
        return Err(format!("{head}: builtin is not yet implemented"));
    }
    let output = process::capture(Command::new(executable).args(arguments), interrupt)
        .map_err(|error| format!("{executable}: {error}"))?;
    if request.lane == DispatchLane::Command {
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

fn scalar(value: &Value) -> Result<String, String> {
    value
        .scalar_text()
        .ok_or_else(|| "expected a scalar".to_owned())
}
