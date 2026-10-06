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
    /// Instruction identity, stable across explicit retries, including when
    /// several members have the same effect kind.
    pub id: usize,
    /// Attempted effect.
    pub kind: XdpCleanupKind,
    /// Successful consumption or diagnostic failure.
    pub outcome: Result<(), E>,
}

/// One pass over owned XDP resources, respecting dependencies.
#[must_use]
pub struct XdpCleanup<R, E> {
    pending: Vec<Pending<R>>,
    report: XdpCleanupReport<R, E>,
}

/// Retained failures and unresolved ownership, ready for caller-budgeted retry.
#[must_use]
pub struct XdpCleanupReport<R, E> {
    remaining: Vec<Pending<R>>,
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
    id: usize,
    kind: XdpCleanupKind,
    operation: XdpCleanup<R, E>,
}

struct Pending<R> {
    id: usize,
    receipt: R,
}

impl<R: XdpResource, E> XdpCleanup<R, E> {
    /// Start a pass. Ordering is policy, independent of acquisition order.
    pub fn new(resources: Vec<R>) -> Self {
        Self::from_pending(
            resources
                .into_iter()
                .enumerate()
                .map(|(id, receipt)| Pending { id, receipt })
                .collect(),
        )
    }

    fn from_pending(mut pending: Vec<Pending<R>>) -> Self {
        pending.sort_by_key(|r| r.receipt.kind());
        pending.reverse();
        Self {
            pending,
            report: XdpCleanupReport {
                remaining: Vec::new(),
                attempts: Vec::new(),
            },
        }
    }

    /// Emit the next independent effect or finish with blocked receipts intact.
    pub fn next(mut self) -> XdpCleanupStep<R, E> {
        while let Some(Pending { id, receipt }) = self.pending.pop() {
            let kind = receipt.kind();
            let blocked = self.report.remaining.iter().any(|r| {
                r.receipt.kind() == XdpCleanupKind::Outer
                    || (kind >= XdpCleanupKind::Directory && r.receipt.kind() <= kind)
            });
            if blocked {
                self.report.remaining.push(Pending { id, receipt });
            } else {
                return XdpCleanupStep::Effect {
                    receipt,
                    next: XdpCleanupContinuation {
                        id,
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
            self.operation.report.remaining.push(Pending {
                id: self.id,
                receipt: failure.remaining,
            });
            failure.cause
        });
        self.operation.report.attempts.push(XdpAttempt {
            id: self.id,
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
        let mut operation = XdpCleanup::from_pending(self.remaining);
        operation.report.attempts = self.attempts;
        operation
    }
}
