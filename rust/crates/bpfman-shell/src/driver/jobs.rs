//! The per-script process registry. Script values hold capabilities only.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    io::Write,
    num::{NonZeroU32, NonZeroU64},
    process::Command,
    sync::mpsc::{self, Receiver, RecvTimeoutError, Sender},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use bpfman_shell_runtime::{Argument, CommandResponse, JobHandle, Value};
use bpfman_shell_semantics::JobSignal;
use bpfman_shell_syntax::parse_go_duration;

use super::{
    files,
    process::{CapturedChild, Interrupt, response},
};

const CHECK_INTERVAL: Duration = Duration::from_millis(5);
const DEFAULT_GRACE: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(super) struct Jobs {
    last_id: u64,
    entries: BTreeMap<NonZeroU64, Job>,
}

impl Jobs {
    pub(super) fn start(
        &mut self,
        arguments: &[Argument],
        origin: String,
    ) -> Result<CommandResponse, String> {
        self.spawn(arguments, origin, Launch::Executable)
    }

    pub(super) fn start_in_namespace(
        &mut self,
        arguments: &[Argument],
        origin: String,
    ) -> Result<CommandResponse, String> {
        self.spawn(arguments, origin, Launch::Namespace)
    }

    pub(super) fn start_fixture(
        &mut self,
        arguments: &[Argument],
        origin: String,
        mode: &'static str,
        publish_binary: bool,
    ) -> Result<CommandResponse, String> {
        self.spawn(
            arguments,
            origin,
            Launch::Fixture {
                mode,
                publish_binary,
            },
        )
    }

    fn spawn(
        &mut self,
        arguments: &[Argument],
        origin: String,
        launch: Launch,
    ) -> Result<CommandResponse, String> {
        let id = self
            .last_id
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .ok_or("exhausted background job identities")?;
        let mut files = files::AdapterFiles::default();
        let argv = arguments
            .iter()
            .map(|argument| files.argument(argument))
            .collect::<Result<Vec<_>, _>>()?;
        let (executable, args) = argv.split_first().ok_or("start requires a command")?;
        let mut command = Command::new(executable);
        command.args(args);
        if let Launch::Fixture { mode, .. } = launch {
            command.env("BPFMAN_SHELL_MODE", mode);
        }
        let process = CapturedChild::spawn(&mut command)
            .map_err(|error| format!("start {executable}: {error}"))?;
        let pid = process.pid().ok_or("background child has no PID")?;
        let target = matches!(
            launch,
            Launch::Executable
                | Launch::Fixture {
                    publish_binary: true,
                    ..
                }
        )
        .then(|| executable.clone());
        let handle = JobHandle::new(id, pid, target);
        let job = Job::spawn(process, files, pid, argv.join(" "), origin)?;

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

    pub(super) fn kill(
        &mut self,
        arguments: &[Argument],
        interrupt: &Interrupt,
    ) -> Result<CommandResponse, String> {
        let mut handle = None;
        let mut explicit = None;
        let mut grace = DEFAULT_GRACE;
        for argument in arguments {
            if let Argument::Value(Value::Job(job)) = argument {
                if handle.replace(job).is_some() {
                    return Err("kill takes one job argument".to_owned());
                }
                continue;
            }
            let text = super::argument(argument)?;
            if let Some(name) = text.strip_prefix("--signal=") {
                explicit =
                    Some(JobSignal::parse(name).ok_or_else(|| format!("unknown signal {name:?}"))?);
            } else if let Some(value) = text.strip_prefix("--grace=") {
                let nanos = parse_go_duration(value)
                    .and_then(|nanos| u64::try_from(nanos).ok())
                    .ok_or_else(|| format!("invalid kill grace period {value:?}"))?;
                grace = Duration::from_nanos(nanos);
            } else {
                return Err(format!("kill: unexpected argument {text:?}"));
            }
        }
        let handle = handle.ok_or("kill requires a job argument")?;
        let job = self.entries.get_mut(&handle.id()).ok_or("unknown job")?;
        if !job.refresh(Duration::ZERO)? {
            if let Some(signal) = explicit {
                job.send(SignalAction::One(signal))?;
            } else {
                job.terminate(grace, interrupt)?;
            }
        }
        job.managed = true;
        Ok(CommandResponse::success(Value::Absent))
    }

    pub(super) fn list(
        &mut self,
        output: Option<&mut dyn Write>,
    ) -> Result<CommandResponse, String> {
        let mut text = String::new();
        for job in self.entries.values_mut().filter(|job| job.listed) {
            job.refresh(Duration::ZERO)?;
            if text.is_empty() {
                writeln!(
                    text,
                    "{:<7} {:<8} {:<12} {:<22} ARGV",
                    "PID", "START", "STATUS", "ORIGIN"
                )
                .map_err(|error| error.to_string())?;
            }
            writeln!(
                text,
                "{:<7} {:<8} {:<12} {:<22} {}",
                job.pid.get(),
                job.started,
                job.status(),
                job.origin,
                job.argv,
            )
            .map_err(|error| error.to_string())?;
        }
        if let Some(output) = output {
            output
                .write_all(text.as_bytes())
                .map_err(|error| error.to_string())?;
        }
        Ok(CommandResponse {
            stdout: text,
            ..CommandResponse::success(Value::Absent)
        })
    }

    pub(super) fn reap(&mut self) -> Result<CommandResponse, String> {
        for job in self.entries.values_mut().filter(|job| job.listed) {
            job.listed = !job.refresh(Duration::ZERO)?;
        }
        Ok(CommandResponse::success(Value::Absent))
    }

    pub(super) fn finish(mut self, result: Result<u8, String>) -> Result<u8, String> {
        let mut leaks = Vec::new();
        let mut failures = Vec::new();
        for job in self.entries.values_mut() {
            if job.listed && !job.managed {
                leaks.push(format!(
                    "[job] FAIL {}: never waited or killed: {}",
                    job.origin, job.argv
                ));
            }
            if let Err(error) = job.stop() {
                failures.push(format!("[job] cleanup {}: {error}", job.origin));
            }
        }
        let leaks_count = leaks.len();
        leaks.extend(failures);
        if leaks.is_empty() {
            return result;
        }
        let mut message = result.err().into_iter().collect::<Vec<_>>();
        if message.is_empty() && leaks_count != 0 {
            leaks.push(format!("{leaks_count} job(s) leaked"));
        }
        message.extend(leaks);
        Err(message.join("\n"))
    }
}

enum Launch {
    Executable,
    Namespace,
    Fixture {
        mode: &'static str,
        publish_binary: bool,
    },
}

enum Control {
    Stop,
    Signal(SignalAction, Sender<Result<bool, String>>),
}

enum SignalAction {
    One(JobSignal),
    TermThenKill,
}

impl SignalAction {
    fn deliver(self, child: &CapturedChild) -> Result<Option<JobSignal>, String> {
        match self {
            Self::One(signal) => child
                .signal(signal)
                .map(|delivered| delivered.then_some(signal)),
            Self::TermThenKill => {
                if child.signal(JobSignal::Term)? {
                    child.signal(JobSignal::Kill)?;
                    Ok(Some(JobSignal::Kill))
                } else {
                    Ok(None)
                }
            }
        }
    }
}

struct Job {
    pid: NonZeroU32,
    argv: String,
    origin: String,
    started: String,
    managed: bool,
    listed: bool,
    signalled: bool,
    outcome: Option<Result<CommandResponse, String>>,
    control: Sender<Control>,
    completed: Receiver<Result<CommandResponse, String>>,
    reaper: Option<JoinHandle<()>>,
}

impl Job {
    fn spawn(
        child: CapturedChild,
        files: files::AdapterFiles,
        pid: NonZeroU32,
        argv: String,
        origin: String,
    ) -> Result<Self, String> {
        let (control, requests) = mpsc::channel();
        let (publish, completed) = mpsc::channel();
        let reaper = thread::Builder::new()
            .name(format!("bpfman-job-{pid}"))
            .spawn(move || {
                let outcome = reap_child(child, requests);
                drop(files);
                // The job table may already have dropped the receiver.
                drop(publish.send(outcome));
            })
            .map_err(|error| format!("start job reaper: {error}"))?;
        Ok(Self {
            pid,
            argv,
            origin,
            started: jiff::Zoned::now().strftime("%H:%M:%S").to_string(),
            managed: false,
            listed: true,
            signalled: false,
            outcome: None,
            control,
            completed,
            reaper: Some(reaper),
        })
    }

    fn stop(&mut self) -> Result<(), String> {
        let Some(reaper) = self.reaper.take() else {
            return Ok(());
        };
        let outcome_was_observed = self.managed && self.outcome.is_some();
        // A disconnected control channel is normal if the child already exited.
        if self.outcome.is_none() {
            drop(self.control.send(Control::Stop));
        }
        reaper
            .join()
            .map_err(|_| "background job reaper panicked".to_owned())?;
        self.refresh(Duration::ZERO)?;
        // A result already inspected by wait is a command outcome, not a
        // newly observed teardown failure.
        if !outcome_was_observed && let Some(Err(error)) = &self.outcome {
            return Err(error.clone());
        }
        Ok(())
    }

    fn refresh(&mut self, delay: Duration) -> Result<bool, String> {
        if self.outcome.is_none() {
            match self.completed.recv_timeout(delay) {
                Ok(outcome) => self.outcome = Some(outcome),
                Err(RecvTimeoutError::Timeout) => return Ok(false),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("background job reaper stopped without a result".to_owned());
                }
            }
        }
        Ok(true)
    }

    fn wait(&mut self, interrupt: &Interrupt) -> Result<CommandResponse, String> {
        loop {
            interrupt.check()?;
            if self.refresh(CHECK_INTERVAL)? {
                self.managed = true;
                return self
                    .outcome
                    .clone()
                    .ok_or("missing job outcome".to_owned())?;
            }
        }
    }

    fn send(&mut self, action: SignalAction) -> Result<(), String> {
        let (reply, observed) = mpsc::channel();
        if self.control.send(Control::Signal(action, reply)).is_ok()
            && observed.recv().unwrap_or(Ok(false))?
        {
            self.signalled = true;
        }
        Ok(())
    }

    fn terminate(&mut self, grace: Duration, interrupt: &Interrupt) -> Result<(), String> {
        if grace.is_zero() {
            self.send(SignalAction::TermThenKill)?;
        } else {
            self.send(SignalAction::One(JobSignal::Term))?;
            let started = Instant::now();
            loop {
                interrupt.check()?;
                let remaining = grace.saturating_sub(started.elapsed());
                if self.refresh(remaining.min(CHECK_INTERVAL))? {
                    return Ok(());
                }
                if remaining.is_zero() {
                    break;
                }
            }
            self.send(SignalAction::One(JobSignal::Kill))?;
        }
        self.wait(interrupt)?;
        Ok(())
    }

    fn status(&self) -> String {
        match &self.outcome {
            Some(Ok(outcome)) => outcome.signal.as_ref().map_or_else(
                || format!("exited {}", outcome.exit_code),
                |signal| format!("killed {signal}"),
            ),
            Some(Err(_)) => "failed".to_owned(),
            None if self.signalled => "killing".to_owned(),
            None => "running".to_owned(),
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // A disconnected receiver means the reaper has already exited.
        drop(self.control.send(Control::Stop));
        if let Some(reaper) = self.reaper.take() {
            super::report_cleanup(
                "join job reaper",
                reaper.join().map_err(|_| "thread panicked"),
            );
        }
    }
}

fn reap_child(
    mut child: CapturedChild,
    requests: Receiver<Control>,
) -> Result<CommandResponse, String> {
    let mut requested = None;
    loop {
        if let Some(output) = child.try_output().map_err(|error| error.to_string())? {
            let mut outcome = response(output);
            if let Some(signal) = requested {
                outcome.killed = true;
                outcome.signal = Some(JobSignal::name(signal).to_owned());
            }
            return Ok(outcome);
        }
        match requests.recv_timeout(CHECK_INTERVAL) {
            Ok(Control::Signal(action, reply)) => {
                let delivered = action.deliver(&child).map(|signal| {
                    if signal.is_some() {
                        requested = signal;
                    }
                    signal.is_some()
                });
                // The caller may have stopped waiting after interruption.
                drop(reply.send(delivered));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Ok(Control::Stop) | Err(RecvTimeoutError::Disconnected) => {
                return child.stop().map(response);
            }
        }
    }
}
