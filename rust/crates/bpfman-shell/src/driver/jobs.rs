//! The per-script process registry. Script values hold capabilities only.

use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
    process::Command,
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

use bpfman_shell_runtime::{Argument, CommandResponse, JobHandle, Value};
use tempfile::NamedTempFile;

use super::{
    files,
    process::{CapturedChild, Interrupt, response},
};

const CHECK_INTERVAL: Duration = Duration::from_millis(5);

#[derive(Default)]
pub(super) struct Jobs {
    last_id: u64,
    entries: BTreeMap<NonZeroU64, Job>,
}

impl Jobs {
    pub(super) fn start(&mut self, arguments: &[Argument]) -> Result<CommandResponse, String> {
        let id = self
            .last_id
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .ok_or("exhausted background job identities")?;
        let mut files = Vec::new();
        let argv = arguments
            .iter()
            .map(|argument| files::external_argument(argument, &mut files))
            .collect::<Result<Vec<_>, _>>()?;
        let (executable, args) = argv.split_first().ok_or("start requires a command")?;
        let process = CapturedChild::spawn(Command::new(executable).args(args))
            .map_err(|error| format!("start {executable}: {error}"))?;
        let pid = process.pid().ok_or("background child has no PID")?;
        let handle = JobHandle::new(id, pid, executable.clone());
        let job = Job::spawn(process, files, pid, executable.clone())?;

        self.last_id = id.get();
        self.entries.insert(id, job);
        Ok(CommandResponse::success(Value::Job(handle)))
    }

    pub(super) fn wait(
        &mut self,
        arguments: &[Argument],
        interrupt: &Interrupt,
    ) -> Result<CommandResponse, String> {
        let [Argument::Value(Value::Job(handle))] = arguments else {
            return Err("wait requires exactly one job argument".to_owned());
        };
        let job = self.entries.get_mut(&handle.id()).ok_or("unknown job")?;
        job.wait(interrupt)
    }

    pub(super) fn finish(self, result: Result<u8, String>) -> Result<u8, String> {
        let leaks: Vec<_> = self
            .entries
            .values()
            .filter(|job| !job.managed)
            .map(|job| format!("unmanaged job {} ({})", job.pid, job.executable))
            .collect();
        if leaks.is_empty() {
            return result;
        }
        let mut message = result.err().into_iter().collect::<Vec<_>>();
        message.extend(leaks);
        Err(message.join("\n"))
    }
}

struct Job {
    pid: NonZeroU32,
    executable: String,
    managed: bool,
    outcome: Option<Result<CommandResponse, String>>,
    stop: Sender<()>,
    completed: Receiver<Result<CommandResponse, String>>,
    reaper: Option<JoinHandle<()>>,
}

impl Job {
    fn spawn(
        child: CapturedChild,
        files: Vec<NamedTempFile>,
        pid: NonZeroU32,
        executable: String,
    ) -> Result<Self, String> {
        let (stop, control) = mpsc::channel();
        let (publish, completed) = mpsc::channel();
        let reaper = thread::Builder::new()
            .name(format!("bpfman-job-{pid}"))
            .spawn(move || {
                let mut child = child;
                let outcome = loop {
                    match child.try_output() {
                        Ok(Some(output)) => break Ok(response(output)),
                        Ok(None) => {}
                        Err(error) => break Err(error.to_string()),
                    }
                    match control.recv_timeout(CHECK_INTERVAL) {
                        Err(RecvTimeoutError::Timeout) => {}
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                    }
                };
                drop(files);
                let _ = publish.send(outcome);
            })
            .map_err(|error| format!("start job reaper: {error}"))?;
        Ok(Self {
            pid,
            executable,
            managed: false,
            outcome: None,
            stop,
            completed,
            reaper: Some(reaper),
        })
    }

    fn wait(&mut self, interrupt: &Interrupt) -> Result<CommandResponse, String> {
        while self.outcome.is_none() {
            interrupt.check()?;
            match self.completed.recv_timeout(CHECK_INTERVAL) {
                Ok(outcome) => self.outcome = Some(outcome),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("background job reaper stopped without a result".to_owned());
                }
            }
        }
        self.managed = true;
        self.outcome
            .clone()
            .ok_or("missing job outcome".to_owned())?
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(reaper) = self.reaper.take() {
            let _ = reaper.join();
        }
    }
}
