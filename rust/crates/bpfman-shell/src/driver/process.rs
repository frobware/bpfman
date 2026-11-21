//! Owned process groups and interruptible waiting for shell commands.

use std::{
    env, fs,
    io::{self, Read},
    num::NonZeroU32,
    os::fd::AsFd,
    os::unix::{
        fs::PermissionsExt,
        process::{CommandExt, ExitStatusExt},
    },
    path::Path,
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
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid},
};
use signal_hook::{
    SigId,
    consts::{SIGINT, SIGTERM},
    flag, low_level,
};
use tempfile::NamedTempFile;

mod terminal;

const INTERRUPT_GRACE: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(5);

pub(crate) struct Interrupt {
    pending: Arc<AtomicBool>,
    handlers: Vec<SigId>,
}

impl Interrupt {
    pub(crate) fn register() -> io::Result<Self> {
        let mut interrupt = Self::unregistered();
        let seen = Arc::new(AtomicBool::new(false));
        for signal in [SIGINT, SIGTERM] {
            interrupt.handlers.push(flag::register_conditional_shutdown(
                signal,
                1,
                Arc::clone(&seen),
            )?);
            interrupt
                .handlers
                .push(flag::register(signal, Arc::clone(&seen))?);
            interrupt
                .handlers
                .push(flag::register(signal, Arc::clone(&interrupt.pending))?);
        }
        Ok(interrupt)
    }

    pub(super) fn unregistered() -> Self {
        Self {
            pending: Arc::new(AtomicBool::new(false)),
            handlers: Vec::new(),
        }
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        if self.pending.swap(false, Ordering::Relaxed) {
            return Err("interrupted by signal".to_owned());
        }
        Ok(())
    }

    pub(crate) fn read_program(&self, mut input: impl AsFd + Read) -> Result<String, String> {
        let timeout = Timespec {
            tv_sec: 0,
            tv_nsec: 20_000_000,
        };
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            self.check()?;
            let mut descriptors = [PollFd::new(&input, PollFlags::IN)];
            match poll(&mut descriptors, Some(&timeout)) {
                Ok(0) | Err(rustix::io::Errno::INTR) => continue,
                Ok(_) => {}
                Err(error) => return Err(error.to_string()),
            }
            self.check()?;
            let count = match input.read(&mut buffer) {
                Ok(count) => count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error.to_string()),
            };
            if count == 0 {
                return String::from_utf8(bytes).map_err(|error| error.utf8_error().to_string());
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
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
    let mut child = CapturedChild::spawn(command).map_err(|error| spawn_error(command, error))?;
    let status = child.group.wait_interruptible(interrupt)?;
    child.output(status).map_err(|error| error.to_string())
}

pub(super) fn foreground(
    command: &mut Command,
    interrupt: &Interrupt,
) -> Result<CommandResponse, String> {
    interrupt.check()?;
    let mut group = Group::spawn(command).map_err(|error| spawn_error(command, error))?;
    let _terminal = group.pid().and_then(terminal::Foreground::grant);
    let status = group.wait_interruptible(interrupt)?;
    Ok(response(Output {
        status,
        stdout: Vec::new(),
        stderr: Vec::new(),
    }))
}

fn spawn_error(command: &Command, error: io::Error) -> String {
    let executable = command.get_program().to_string_lossy();
    if error.kind() == io::ErrorKind::NotFound {
        format!("{executable}: command not found")
    } else {
        format!("{executable}: {error}")
    }
}

pub(super) fn has_executable(executable: &str) -> bool {
    let runnable = |path: &Path| {
        fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    };
    if executable.contains('/') {
        return runnable(Path::new(executable));
    }
    env::var_os("PATH")
        .is_some_and(|paths| env::split_paths(&paths).any(|path| runnable(&path.join(executable))))
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
        command
            .stdin(Stdio::null())
            .stdout(stdout.reopen()?)
            .stderr(stderr.reopen()?);
        Ok(Self {
            group: Group::spawn(command)?,
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

    pub(super) fn stop(&mut self) -> Result<Output, String> {
        let status = self.group.stop()?;
        self.output(status).map_err(|error| error.to_string())
    }

    pub(super) fn try_output(&mut self) -> io::Result<Option<Output>> {
        let Some(status) = self.group.try_wait()? else {
            return Ok(None);
        };
        self.output(status).map(Some)
    }

    fn output(&self, status: ExitStatus) -> io::Result<Output> {
        Ok(Output {
            status,
            stdout: read_capture(&self.stdout)?,
            stderr: read_capture(&self.stderr)?,
        })
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
    fn spawn(command: &mut Command) -> io::Result<Self> {
        command
            .process_group(0)
            .spawn()
            .map(|child| Self(Some(child)))
    }

    fn wait_interruptible(&mut self, interrupt: &Interrupt) -> Result<ExitStatus, String> {
        let mut cancelled = None;
        loop {
            if cancelled.is_none()
                && let Err(mut error) = interrupt.check()
            {
                if let Some(pid) = self.pid()
                    && let Err(signal_error) = kill_process_group(pid, Signal::INT)
                    && signal_error != rustix::io::Errno::SRCH
                {
                    error.push_str(&format!("; interrupt process group: {signal_error}"));
                }
                cancelled = Some((Instant::now() + INTERRUPT_GRACE, error));
            }
            if let Some((deadline, error)) = &cancelled {
                // Keep the leader's PID reserved until the ownership guard
                // can finish killing and reaping the group.
                let exited = self.pid().is_some_and(|pid| {
                    waitid(
                        WaitId::Pid(pid),
                        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
                    )
                    .is_ok_and(|status| status.is_some())
                });
                if exited || Instant::now() >= *deadline {
                    return Err(error.clone());
                }
            } else if let Some(status) = self.try_wait().map_err(|error| error.to_string())? {
                return Ok(status);
            }
            thread::sleep(POLL);
        }
    }

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

impl Group {
    fn stop(&mut self) -> Result<ExitStatus, String> {
        let pid = self.pid();
        let mut child = self.0.take().ok_or("process already joined")?;
        if let Some(pid) = pid {
            match kill_process_group(pid, Signal::KILL) {
                Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                Err(error) => return Err(format!("kill process group {pid}: {error}")),
            }
        } else {
            child
                .kill()
                .map_err(|error| format!("kill child: {error}"))?;
        }
        child
            .wait()
            .map_err(|error| format!("wait for child: {error}"))
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if self.0.is_some() {
            super::report_cleanup("stop child process group", self.stop());
        }
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
