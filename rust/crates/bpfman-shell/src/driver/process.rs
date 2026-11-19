//! Owned process groups and interruptible waiting for shell commands.

use std::{
    io::{self, Read},
    os::unix::process::CommandExt,
    process::{Child, Command, ExitStatus, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

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
    let (mut group, stdout, stderr) = spawn(command).map_err(|error| error.to_string())?;
    let status = loop {
        interrupt.check()?;
        if let Some(status) = group.try_wait().map_err(|error| error.to_string())? {
            break status;
        }
        thread::sleep(POLL);
    };
    Ok(Output {
        status,
        stdout: read_capture(&stdout).map_err(|error| error.to_string())?,
        stderr: read_capture(&stderr).map_err(|error| error.to_string())?,
    })
}

fn spawn(command: &mut Command) -> io::Result<(Group, NamedTempFile, NamedTempFile)> {
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
    Ok((Group(Some(child)), stdout, stderr))
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
        let Some(child) = self.0.as_mut() else {
            return;
        };
        if let Some(pid) = i32::try_from(child.id()).ok().and_then(Pid::from_raw) {
            let _ = kill_process_group(pid, Signal::KILL);
        } else {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}
