//! Ordered receipt cleanup for a first XDP attachment or last detach.

use crate::EffectFailure;
use alloc::vec::Vec;

/// Dependency-ordered XDP teardown effects.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum XdpCleanupKind {
    /// Synchronously detach the interface link, then remove its pin.
    Outer,
    /// Remove the extension link after traffic has stopped.
    Extension,
    /// Remove the dispatcher program pin after traffic has stopped.
    Program,
    /// Remove the revision directory after both children are gone.
    Directory,
    /// Delete the unchanged store snapshot after artifacts are gone.
    Record,
}

/// Opaque, owned adapter evidence identifying a conceptual teardown effect.
pub trait XdpResource {
    /// Effect authorized by this receipt; never a path-based removal command.
    fn kind(&self) -> XdpCleanupKind;
}

/// Outcome of an actual effect; blocked work is retained without a fake attempt.
#[derive(Debug)]
pub struct XdpAttempt<E> {
    /// Attempted effect.
    pub kind: XdpCleanupKind,
    /// Successful consumption or diagnostic failure.
    pub outcome: Result<(), E>,
}

/// One pass over owned XDP resources, respecting dependencies.
#[must_use]
pub struct XdpCleanup<R, E> {
    pending: Vec<R>,
    report: XdpCleanupReport<R, E>,
}

/// Retained failures and unresolved ownership, ready for caller-budgeted retry.
#[must_use]
pub struct XdpCleanupReport<R, E> {
    remaining: Vec<R>,
    attempts: Vec<XdpAttempt<E>>,
}

/// Owned next instruction with consuming continuation, or completed pass.
pub enum XdpCleanupStep<R, E> {
    /// Execute this conceptual resource operation exactly once.
    Effect {
        /// Adapter-owned evidence.
        receipt: R,
        /// Result continuation.
        next: XdpCleanupContinuation<R, E>,
    },
    /// All independent work attempted; blocked ownership is retained.
    Complete(XdpCleanupReport<R, E>),
}

/// Consumes one success or failure, never retrying inline.
pub struct XdpCleanupContinuation<R, E> {
    kind: XdpCleanupKind,
    operation: XdpCleanup<R, E>,
}

impl<R: XdpResource, E> XdpCleanup<R, E> {
    /// Start a pass. Ordering is policy, independent of acquisition order.
    pub fn new(mut resources: Vec<R>) -> Self {
        resources.sort_by_key(XdpResource::kind);
        resources.reverse();
        Self {
            pending: resources,
            report: XdpCleanupReport {
                remaining: Vec::new(),
                attempts: Vec::new(),
            },
        }
    }

    /// Emit the next independent effect or finish with blocked receipts intact.
    pub fn next(mut self) -> XdpCleanupStep<R, E> {
        while let Some(receipt) = self.pending.pop() {
            let kind = receipt.kind();
            let blocked = self.report.remaining.iter().any(|r| {
                r.kind() == XdpCleanupKind::Outer
                    || (kind >= XdpCleanupKind::Directory && r.kind() <= kind)
            });
            if blocked {
                self.report.remaining.push(receipt);
            } else {
                return XdpCleanupStep::Effect {
                    receipt,
                    next: XdpCleanupContinuation {
                        kind,
                        operation: self,
                    },
                };
            }
        }
        XdpCleanupStep::Complete(self.report)
    }
}

impl<R, E> XdpCleanupContinuation<R, E> {
    /// Record the outcome and preserve failed ownership.
    pub fn completed(mut self, result: Result<(), EffectFailure<R, E>>) -> XdpCleanup<R, E> {
        let outcome = result.map_err(|failure| {
            self.operation.report.remaining.push(failure.remaining);
            failure.cause
        });
        self.operation.report.attempts.push(XdpAttempt {
            kind: self.kind,
            outcome,
        });
        self.operation
    }
}

impl<R: XdpResource, E> XdpCleanupReport<R, E> {
    /// Count of unresolved operations, including blocked dependents.
    pub fn unresolved(&self) -> usize {
        self.remaining.len()
    }

    /// Every attempted effect over all passes.
    pub fn attempts(&self) -> &[XdpAttempt<E>] {
        &self.attempts
    }

    /// Start one new pass over only unresolved receipts.
    pub fn retry(self) -> XdpCleanup<R, E> {
        let mut operation = XdpCleanup::new(self.remaining);
        operation.report.attempts = self.attempts;
        operation
    }
}
