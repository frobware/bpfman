use std::time::Duration;

use bpfman_shell_ir::{BlockId, PollSchedule, RetryTarget, Temp};
use bpfman_shell_source::Span;

use super::{Action, Machine, Step, Value};

pub(super) struct Poll {
    pub(super) owner: BlockId,
    pub(super) span: Span,
    attempt: BlockId,
    timeout: BlockId,
    pub(super) success: BlockId,
    deadline: Duration,
    every: Duration,
    frames: usize,
    loops: usize,
    pub(super) deferred_base: usize,
    attempts: usize,
    last_message: Option<String>,
}

pub(super) struct RetrySignal {
    pub(super) message: Option<String>,
    pub(super) span: Span,
}

pub(super) enum ClockPurpose {
    Start {
        schedule: PollSchedule,
        attempt: BlockId,
        timeout: BlockId,
        success: BlockId,
    },
    Retry,
}

/// The state suspended while reading a monotonic clock.
pub struct ClockContinuation<'a> {
    pub(super) machine: Machine<'a>,
    pub(super) purpose: ClockPurpose,
    pub(super) span: Span,
}

impl<'a> ClockContinuation<'a> {
    /// Continue with the driver's monotonic elapsed time.
    pub fn resume(mut self, observation: Result<Duration, String>) -> Step<'a> {
        let now = match observation {
            Ok(now) => now,
            Err(error) => {
                self.machine.error(&self.span, error);
                return self.machine.advance();
            }
        };
        match self.purpose {
            ClockPurpose::Start {
                schedule,
                attempt,
                timeout,
                success,
            } => {
                self.machine.polls.push(Poll {
                    owner: self.machine.block,
                    span: self.span.clone(),
                    attempt,
                    timeout,
                    success,
                    deadline: now.saturating_add(Duration::from_nanos(schedule.timeout.get())),
                    every: Duration::from_nanos(schedule.every.get()),
                    frames: self.machine.frames.len(),
                    loops: self.machine.loops.len(),
                    deferred_base: self.machine.deferred.len(),
                    attempts: 1,
                    last_message: None,
                });
                self.machine.jump(attempt);
            }
            ClockPurpose::Retry => {
                let Some(poll) = self.machine.polls.last_mut() else {
                    self.machine
                        .error(&self.span, "retry outside a poll".to_owned());
                    return self.machine.advance();
                };
                if now >= poll.deadline {
                    let timeout = poll.timeout;
                    self.machine.jump(timeout);
                } else {
                    poll.attempts = poll.attempts.saturating_add(1);
                    let duration = poll.every;
                    let attempt = poll.attempt;
                    return Step::Pause {
                        duration,
                        continuation: PauseContinuation {
                            machine: self.machine,
                            attempt,
                            span: self.span,
                        },
                    };
                }
            }
        }
        self.machine.advance()
    }
}

/// The state suspended while waiting for the next attempt.
pub struct PauseContinuation<'a> {
    machine: Machine<'a>,
    attempt: BlockId,
    span: Span,
}

impl<'a> PauseContinuation<'a> {
    /// Continue when the delay has elapsed or the driver interrupts it.
    pub fn resume(mut self, observation: Result<(), String>) -> Step<'a> {
        match observation {
            Ok(()) => self.machine.jump(self.attempt),
            Err(error) => self.machine.error(&self.span, error),
        }
        self.machine.advance()
    }
}

impl Machine<'_> {
    pub(super) fn retry_poll(
        &mut self,
        target: RetryTarget,
        message: Option<Temp>,
        span: &Span,
    ) -> Result<Action, String> {
        let message = message
            .map(|temp| {
                self.value(temp).map(|value| match value {
                    Value::Absent => String::new(),
                    Value::Null => "null".to_owned(),
                    Value::Bool(value) => value.to_string(),
                    Value::Number(value) => value.as_str().to_owned(),
                    Value::String(text) => text.clone(),
                    _ => "<unrenderable retry message>".to_owned(),
                })
            })
            .transpose()?;
        if let Some(poll) = self.polls.last() {
            let origin = poll.span.clone();
            self.trace_note(&origin, "poll retry");
        }
        match target {
            RetryTarget::Local(owner) => {
                self.prepare_retry(owner, message)?;
                Ok(Action::Clock(ClockPurpose::Retry))
            }
            RetryTarget::Caller => {
                self.propagate_retry(RetrySignal {
                    message,
                    span: span.clone(),
                });
                Ok(Action::Next)
            }
        }
    }

    pub(super) fn prepare_retry(
        &mut self,
        owner: BlockId,
        message: Option<String>,
    ) -> Result<(), String> {
        let poll = self
            .polls
            .last_mut()
            .filter(|poll| poll.owner == owner)
            .ok_or("retry owner is not active")?;
        poll.last_message = message;
        self.frames.truncate(poll.frames);
        self.loops.truncate(poll.loops);
        Ok(())
    }

    pub(super) fn poll_timeout(&mut self) -> Option<String> {
        let poll = self
            .polls
            .last()
            .filter(|poll| poll.timeout == self.block)?;
        let mut message = format!("poll timed out after {} attempt(s)", poll.attempts);
        if let Some(reason) = &poll.last_message {
            message.push_str(": ");
            message.push_str(reason);
        }
        self.polls.pop();
        Some(message)
    }
}
