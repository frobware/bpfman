//! Owned process groups and interruptible waiting for shell commands.

use std::{
    io::{self, Read},
    num::NonZeroU32,
    os::unix::process::{CommandExt, ExitStatusExt},
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use bpfman_shell_runtime::{CommandResponse, Value};
use bpfman_shell_semantics::JobSignal;
use rustix::process::{Pid, Signal, kill_process_group};
use signal_hook::{
    SigId,
    consts::{SIGINT, SIGTERM},
    flag, low_level,
};
use tempfile::NamedTempFile;

const POLL: Duration = Duration::from_millis(5);

pub(super) struct Interrupt {
    pending: Arc<AtomicBool>,
    handlers: Vec<SigId>,
}

impl Interrupt {
    pub(super) fn register() -> io::Result<Self> {
        let mut interrupt = Self {
            pending: Arc::new(AtomicBool::new(false)),
            handlers: Vec::new(),
        };
        for signal in [SIGINT, SIGTERM] {
            interrupt
                .handlers
                .push(flag::register(signal, Arc::clone(&interrupt.pending))?);
        }
        Ok(interrupt)
    }

    pub(super) fn check(&self) -> Result<(), String> {
        if self.pending.swap(false, Ordering::Relaxed) {
            return Err("interrupted by signal".to_owned());
        }
        Ok(())
    }

    pub(super) fn pause(&self, duration: Duration) -> Result<(), String> {
        let start = Instant::now();
        loop {
            self.check()?;
            let remaining = duration.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Ok(());
            }
            thread::sleep(remaining.min(POLL));
        }
    }
}

impl Drop for Interrupt {
    fn drop(&mut self) {
        for handler in self.handlers.drain(..) {
            low_level::unregister(handler);
        }
    }
}

pub(super) fn capture(command: &mut Command, interrupt: &Interrupt) -> Result<Output, String> {
    interrupt.check()?;
    let mut child = CapturedChild::spawn(command).map_err(|error| error.to_string())?;
    loop {
        interrupt.check()?;
        if let Some(output) = child.try_output().map_err(|error| error.to_string())? {
            return Ok(output);
        }
        thread::sleep(POLL);
    }
}

pub(super) fn response(output: Output) -> CommandResponse {
    let signal = output.status.signal();
    let code = output
        .status
        .code()
        .unwrap_or_else(|| 128 + signal.unwrap_or(0));
    CommandResponse {
        exit_code: u8::try_from(code).unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        signal: signal.map(signal_name),
        ..CommandResponse::success(Value::Absent)
    }
}

pub(super) struct CapturedChild {
    group: Group,
    stdout: NamedTempFile,
    stderr: NamedTempFile,
}

impl CapturedChild {
    pub(super) fn spawn(command: &mut Command) -> io::Result<Self> {
        // Files let the child make progress even if both streams fill.
        // Reopening also gives the reader an independent file offset.
        let stdout = tempfile::NamedTempFile::new()?;
        let stderr = tempfile::NamedTempFile::new()?;
        let child = command
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(stdout.reopen()?)
            .stderr(stderr.reopen()?)
            .spawn()?;
        Ok(Self {
            group: Group(Some(child)),
            stdout,
            stderr,
        })
    }

    pub(super) fn pid(&self) -> Option<NonZeroU32> {
        self.group
            .0
            .as_ref()
            .and_then(|child| NonZeroU32::new(child.id()))
    }

    pub(super) fn signal(&self, signal: JobSignal) -> Result<bool, String> {
        let Some(pid) = self.group.pid() else {
            return Ok(false);
        };
        match kill_process_group(pid, syscall_signal(signal)) {
            Ok(()) => Ok(true),
            Err(rustix::io::Errno::SRCH) => Ok(false),
            Err(error) => Err(format!("kill process group {pid}: {error}")),
        }
    }

    pub(super) fn try_output(&mut self) -> io::Result<Option<Output>> {
        let Some(status) = self.group.try_wait()? else {
            return Ok(None);
        };
        Ok(Some(Output {
            status,
            stdout: read_capture(&self.stdout)?,
            stderr: read_capture(&self.stderr)?,
        }))
    }
}

fn read_capture(file: &NamedTempFile) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let reader = file.reopen()?;
    let length = reader.metadata()?.len();
    reader.take(length).read_to_end(&mut bytes)?;
    Ok(bytes)
}

struct Group(Option<Child>);

impl Group {
    fn pid(&self) -> Option<Pid> {
        let child = self.0.as_ref()?;
        i32::try_from(child.id()).ok().and_then(Pid::from_raw)
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let child = self
            .0
            .as_mut()
            .ok_or_else(|| io::Error::other("process already joined"))?;
        let status = child.try_wait()?;
        if status.is_some() {
            self.0 = None;
        }
        Ok(status)
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        let pid = self.pid();
        let Some(child) = self.0.as_mut() else {
            return;
        };
        if let Some(pid) = pid {
            let _ = kill_process_group(pid, Signal::KILL);
        } else {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}

fn syscall_signal(signal: JobSignal) -> Signal {
    match signal {
        JobSignal::Term => Signal::TERM,
        JobSignal::Kill => Signal::KILL,
        JobSignal::Int => Signal::INT,
        JobSignal::Quit => Signal::QUIT,
        JobSignal::Hup => Signal::HUP,
        JobSignal::Usr1 => Signal::USR1,
        JobSignal::Usr2 => Signal::USR2,
        JobSignal::Stop => Signal::STOP,
        JobSignal::Cont => Signal::CONT,
    }
}

fn signal_name(raw: i32) -> String {
    JobSignal::ALL
        .into_iter()
        .find(|signal| syscall_signal(*signal).as_raw() == raw)
        .map_or_else(|| raw.to_string(), |signal| signal.name().to_owned())
}
