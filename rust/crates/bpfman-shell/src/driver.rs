use std::{
    collections::HashMap,
    io::{self, Write},
    os::unix::process::ExitStatusExt,
    process::Command,
    time::Instant,
};

use bpfman_shell_ir::Program;
use bpfman_shell_runtime::{
    Argument, CommandRequest, CommandResponse, DispatchLane, Step, Value, execute,
};

pub(super) fn run(program: &Program, stdout: &mut impl Write) -> Result<u8, String> {
    let start = Instant::now();
    let mut step = execute(program, HashMap::new());
    loop {
        step =
            match step {
                Step::Command {
                    request,
                    continuation,
                } => continuation.resume(dispatch(request, stdout)),
                Step::Output {
                    request,
                    continuation,
                } => continuation.resume(scalar(&request.value).and_then(|value| {
                    writeln!(stdout, "{value}").map_err(|error| error.to_string())
                })),
                Step::ReadClock(continuation) => continuation.resume(Ok(start.elapsed())),
                Step::Pause {
                    duration,
                    continuation,
                } => {
                    std::thread::sleep(duration);
                    continuation.resume(Ok(()))
                }
                Step::Finished { exit_code } => return Ok(exit_code),
                Step::Failed(error) => return Err(error.to_string()),
            };
    }
}

fn dispatch(request: CommandRequest, stdout: &mut impl Write) -> Result<CommandResponse, String> {
    let head = argument(&request.head)?;
    let arguments = request
        .arguments
        .iter()
        .map(argument)
        .collect::<Result<Vec<_>, _>>()?;
    if head == "print" {
        let line = arguments.join(" ");
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
            "bpfman"
                | "defs"
                | "file"
                | "fire"
                | "import"
                | "jobs"
                | "jq"
                | "kfunc"
                | "kill"
                | "linkinfo"
                | "lsm"
                | "net"
                | "process"
                | "proginfo"
                | "range"
                | "reap"
                | "registry"
                | "start"
                | "tempdir"
                | "trace"
                | "u32le"
                | "u64le"
                | "uprobe"
                | "uprobe-target"
                | "wait"
                | "zip"
        )
    {
        return Err(format!("{head}: builtin is not yet implemented"));
    }
    let output = Command::new(executable)
        .args(arguments)
        .output()
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
    let signal = output.status.signal();
    let code = output
        .status
        .code()
        .unwrap_or_else(|| 128 + signal.unwrap_or(0));
    Ok(CommandResponse {
        exit_code: u8::try_from(code).unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        primary: Value::Absent,
        killed: false,
        signal: signal.map(|value| value.to_string()),
        pid: None,
    })
}

fn argument(argument: &Argument) -> Result<String, String> {
    match argument {
        Argument::Word(text) | Argument::Quoted(text) => Ok(text.clone()),
        Argument::Value(value) => scalar(value),
    }
}

fn scalar(value: &Value) -> Result<String, String> {
    match value {
        Value::Absent => Err("expected a value".to_owned()),
        Value::Null => Ok("null".to_owned()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(value.as_str().to_owned()),
        Value::String(value) => Ok(value.clone()),
        _ => Err("expected a scalar".to_owned()),
    }
}
