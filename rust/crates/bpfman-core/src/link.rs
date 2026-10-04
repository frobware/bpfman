//! One dependent cleanup chain, shared by failed attachment and explicit detach.

use crate::EffectFailure;
use alloc::vec::Vec;

/// Owned attachment state. A pinned link needs unpinning; a transient one needs
/// its live handle released. Neither resource can be silently discarded by policy.
pub enum LinkResource<L, P> {
    /// Live attachment not yet pinned.
    Live(L),
    /// Owned persistent pin.
    Pin(P),
}

/// Stable labels for attempted link teardown effects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkCleanupKind {
    /// Release an unpinned live attachment.
    ReleaseLive,
    /// Remove a persistent attachment pin.
    RemovePin,
    /// Delete the unchanged link record after releasing the attachment.
    DeleteRecord,
}

/// An actual attempt, including successful steps and earlier retry passes.
#[derive(Debug)]
pub struct LinkAttempt<E> {
    /// Effect attempted.
    pub kind: LinkCleanupKind,
    /// Successful completion or its failure cause.
    pub outcome: Result<(), E>,
}

/// One cleanup pass. Record deletion depends on successful attachment release.
#[must_use = "execute cleanup or retain the owned receipts"]
pub struct LinkCleanup<L, P, R, E> {
    report: LinkCleanupReport<L, P, R, E>,
    blocked: bool,
}

/// Attempt history and unresolved ownership, including blocked record deletion.
#[must_use = "inspect progress and retain unresolved cleanup"]
pub struct LinkCleanupReport<L, P, R, E> {
    resource: Option<LinkResource<L, P>>,
    record: Option<R>,
    attempts: Vec<LinkAttempt<E>>,
}

/// Next effect, carrying its owned receipt and consuming continuation.
pub enum LinkCleanupStep<L, P, R, E> {
    /// Release a live attachment.
    Live {
        /// Owned live handle.
        receipt: L,
        /// Continuation for this attempt.
        next: LinkCleanupContinuation<L, L, P, R, E>,
    },
    /// Unpin a persistent attachment.
    Pin {
        /// Owned pin.
        receipt: P,
        /// Continuation for this attempt.
        next: LinkCleanupContinuation<P, L, P, R, E>,
    },
    /// Delete the record, after its attachment prerequisite succeeds.
    Record {
        /// Conditional store evidence.
        receipt: R,
        /// Continuation for this attempt.
        next: LinkCleanupContinuation<R, L, P, R, E>,
    },
    /// Pass completed, possibly retaining unresolved work.
    Complete(LinkCleanupReport<L, P, R, E>),
}

/// Consume an effect result exactly once; failure never causes an inline retry.
pub struct LinkCleanupContinuation<T, L, P, R, E> {
    operation: LinkCleanup<L, P, R, E>,
    kind: LinkCleanupKind,
    retain: fn(&mut LinkCleanupReport<L, P, R, E>, T),
}

impl<L, P, R, E> LinkCleanup<L, P, R, E> {
    /// Begin one pass, with no kernel work when the attachment is already absent.
    pub fn new(resource: Option<LinkResource<L, P>>, record: R) -> Self {
        Self {
            report: LinkCleanupReport {
                resource,
                record: Some(record),
                attempts: Vec::new(),
            },
            blocked: false,
        }
    }

    /// Emit the next admissible effect. A failed prerequisite blocks deletion
    /// without fabricating a failed deletion attempt.
    pub fn next(mut self) -> LinkCleanupStep<L, P, R, E> {
        if self.blocked {
            return LinkCleanupStep::Complete(self.report);
        }

        if let Some(resource) = self.report.resource.take() {
            return match resource {
                LinkResource::Live(receipt) => LinkCleanupStep::Live {
                    receipt,
                    next: LinkCleanupContinuation {
                        operation: self,
                        kind: LinkCleanupKind::ReleaseLive,
                        retain: |report, value| report.resource = Some(LinkResource::Live(value)),
                    },
                },
                LinkResource::Pin(receipt) => LinkCleanupStep::Pin {
                    receipt,
                    next: LinkCleanupContinuation {
                        operation: self,
                        kind: LinkCleanupKind::RemovePin,
                        retain: |report, value| report.resource = Some(LinkResource::Pin(value)),
                    },
                },
            };
        }

        if let Some(receipt) = self.report.record.take() {
            return LinkCleanupStep::Record {
                receipt,
                next: LinkCleanupContinuation {
                    operation: self,
                    kind: LinkCleanupKind::DeleteRecord,
                    retain: |report, value| report.record = Some(value),
                },
            };
        }

        LinkCleanupStep::Complete(self.report)
    }
}

impl<T, L, P, R, E> LinkCleanupContinuation<T, L, P, R, E> {
    /// Retain failed ownership and append this outcome to the complete history.
    pub fn completed(mut self, result: Result<(), EffectFailure<T, E>>) -> LinkCleanup<L, P, R, E> {
        let outcome = result.map_err(|failure| {
            (self.retain)(&mut self.operation.report, failure.remaining);
            self.operation.blocked = true;
            failure.cause
        });
        self.operation.report.attempts.push(LinkAttempt {
            kind: self.kind,
            outcome,
        });

        self.operation
    }
}

impl<L, P, R, E> LinkCleanupReport<L, P, R, E> {
    /// Number of unresolved effects, including blocked record deletion.
    pub fn unresolved(&self) -> usize {
        usize::from(self.resource.is_some()) + usize::from(self.record.is_some())
    }

    /// Actual attempts, including successes and previous passes.
    pub fn attempts(&self) -> &[LinkAttempt<E>] {
        &self.attempts
    }

    /// Begin one caller-budgeted pass over only unresolved work.
    pub fn retry(self) -> LinkCleanup<L, P, R, E> {
        LinkCleanup {
            report: self,
            blocked: false,
        }
    }
}
