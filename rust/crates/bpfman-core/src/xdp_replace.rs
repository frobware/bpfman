//! Ownership transitions across a live-link switch and atomic store publication.
//!
//! The interpreter stages a complete revision before entering this protocol.
//! Adapter-owned revision and switch receipts stay opaque. In particular, a
//! failed switch must distinguish rejection from mutation: only the latter
//! grants restoration evidence. Restoration validates the actual live target
//! before allowing staged resources to enter the ordinary XDP cleanup machine.

use crate::EffectFailure;
use alloc::vec::Vec;

/// Complete old and staged revisions, ready to switch the durable outer link.
/// Neither revision is eligible for cleanup while the switch is in flight.
#[must_use = "report the switch outcome without discarding either revision"]
pub struct XdpReplacement<O, N> {
    old: O,
    staged: N,
}

/// The live target changed; publication or restoration must decide ownership.
///
/// A commit cannot occur before switching:
///
/// ```compile_fail
/// use bpfman_core::XdpReplacement;
/// XdpReplacement::new((), ()).committed(());
/// ```
#[must_use = "publish the complete snapshot or restore the previous live target"]
pub struct XdpPublication<O, N, S> {
    replacement: XdpReplacement<O, N>,
    switch: S,
}

/// A rejected switch, or a restored old target. Only `cleanup` may be removed.
/// Cleanup errors must be retained in addition to this original failure.
#[must_use = "retain the authoritative revision and compensate staged ownership"]
pub struct XdpReplacementRollback<O, N, E> {
    /// Previous revision; still authoritative in both kernel and store.
    pub retained: O,
    /// Inactive staged revision, eligible for dependency-ordered cleanup.
    pub cleanup: N,
    /// Original switch, publication, or cancellation failure.
    pub primary: E,
    /// All restoration attempts, including failures from previous passes.
    pub restoration_attempts: Vec<Result<(), E>>,
}

/// Publication succeeded. Failure cleaning up the old revision cannot restore it.
///
/// ```compile_fail
/// use bpfman_core::XdpReplacement;
/// let committed = XdpReplacement::new((), ()).switched(()).committed(());
/// committed.failed("late cancellation");
/// ```
#[must_use = "retain committed ownership and retire only the old revision"]
pub struct XdpRetirement<O, N, S, C> {
    /// Old, inactive revision eligible for dependency-ordered cleanup.
    pub cleanup: O,
    /// Newly authoritative revision; never part of compensation.
    pub retained: N,
    /// Switch evidence returned to the interpreter for release after commit.
    pub switch: S,
    /// Actual successful publication result, even if cancellation arrived in flight.
    pub committed: C,
}

/// Restoration must precede any attempt to delete the staged revision.
///
/// ```compile_fail
/// use bpfman_core::XdpReplacement;
/// let restore = XdpReplacement::new((), ()).switched(()).failed("publication");
/// let staged = restore.staged;
/// ```
#[must_use = "execute restoration and retain both revisions on failure"]
pub struct XdpRestoration<O, N, S, E> {
    replacement: XdpReplacement<O, N>,
    switch: S,
    primary: E,
    attempts: Vec<Result<(), E>>,
}

/// One restoration effect. The adapter consumes its evidence on success and
/// returns it on failure. The continuation hides both revisions from cleanup.
#[must_use = "execute the restoration effect and report its actual outcome"]
pub struct XdpRestoreStep<O, N, S, E> {
    /// Adapter-owned evidence binding the outer link, runtime, and both targets.
    pub receipt: S,
    /// Consuming continuation accepting the same evidence type on failure.
    pub next: XdpRestoreContinuation<O, N, S, E>,
}

/// Both revisions remain inaccessible to cleanup until restoration succeeds.
/// The continuation only accepts the restoration receipt type it dispatched:
///
/// ```compile_fail
/// use bpfman_core::{EffectFailure, XdpReplacement};
/// struct Switch;
/// struct ProgramPin;
/// let step = XdpReplacement::new((), ()).switched(Switch).failed("store").restore();
/// step.next.completed(Err(EffectFailure { remaining: ProgramPin, cause: "restore" }));
/// ```
///
/// A completed continuation cannot be replayed:
///
/// ```compile_fail
/// use bpfman_core::XdpReplacement;
/// let step = XdpReplacement::new((), ()).switched(()).failed("store").restore();
/// let result = step.next.completed(Ok(()));
/// let duplicate = step.next.completed(Ok(()));
/// ```
#[must_use = "complete the restoration attempt"]
pub struct XdpRestoreContinuation<O, N, S, E> {
    receipt: core::marker::PhantomData<fn(S) -> S>,
    replacement: XdpReplacement<O, N>,
    primary: E,
    attempts: Vec<Result<(), E>>,
}

/// Failed restoration blocks all revision cleanup. The caller may explicitly
/// retry under the original runtime authority; no automatic retry is performed.
#[must_use = "retain restoration evidence and report the unresolved operation"]
pub struct XdpRestoreFailure<O, N, S, E> {
    restoration: XdpRestoration<O, N, S, E>,
}

impl<O, N> XdpReplacement<O, N> {
    /// Begin after staging every desired member and validating the old target.
    /// Inputs must contain owned receipts, not paths granting deletion authority.
    pub fn new(old: O, staged: N) -> Self {
        Self { old, staged }
    }

    /// Borrow the retained old revision to execute a conditional switch.
    pub fn old(&self) -> &O {
        &self.old
    }

    /// Borrow the complete staged revision to execute a conditional switch.
    pub fn staged(&self) -> &N {
        &self.staged
    }

    /// Rejection before mutation, or cancellation before starting the switch.
    pub fn rejected<E>(self, primary: E) -> XdpReplacementRollback<O, N, E> {
        XdpReplacementRollback {
            retained: self.old,
            cleanup: self.staged,
            primary,
            restoration_attempts: Vec::new(),
        }
    }

    /// Successful live-target update. Store publication is now permitted.
    pub fn switched<S>(self, switch: S) -> XdpPublication<O, N, S> {
        XdpPublication {
            replacement: self,
            switch,
        }
    }

    /// The adapter changed the live target before failing. Treat its retained
    /// ownership exactly like a publication failure; never clean it immediately.
    pub fn switch_failed<S, E>(self, failure: EffectFailure<S, E>) -> XdpRestoration<O, N, S, E> {
        self.switched(failure.remaining).failed(failure.cause)
    }
}

impl<O, N, S> XdpPublication<O, N, S> {
    /// Borrow the staged revision to construct the atomic snapshot publication.
    pub fn staged(&self) -> &N {
        &self.replacement.staged
    }

    /// Store success ends compensation authority, including late cancellation.
    pub fn committed<C>(self, committed: C) -> XdpRetirement<O, N, S, C> {
        XdpRetirement {
            cleanup: self.replacement.old,
            retained: self.replacement.staged,
            switch: self.switch,
            committed,
        }
    }

    /// Failed publication, or cancellation before publication starts, requires
    /// restoration. An in-flight commit must report its real outcome first.
    pub fn failed<E>(self, primary: E) -> XdpRestoration<O, N, S, E> {
        XdpRestoration {
            replacement: self.replacement,
            switch: self.switch,
            primary,
            attempts: Vec::new(),
        }
    }
}

impl<O, N, S, E> XdpRestoration<O, N, S, E> {
    /// Execute exactly one restoration attempt, even if forward cancellation
    /// was requested. Failed admission should retain this value unchanged.
    pub fn restore(self) -> XdpRestoreStep<O, N, S, E> {
        XdpRestoreStep {
            receipt: self.switch,
            next: XdpRestoreContinuation {
                receipt: core::marker::PhantomData,
                replacement: self.replacement,
                primary: self.primary,
                attempts: self.attempts,
            },
        }
    }
}

type RestoreResult<O, N, S, E> =
    Result<XdpReplacementRollback<O, N, E>, XdpRestoreFailure<O, N, S, E>>;

impl<O, N, S, E> XdpRestoreContinuation<O, N, S, E> {
    /// Success unlocks staged cleanup. Failure retains both revisions, the
    /// original error, the restoration error, and evidence for explicit retry.
    pub fn completed(
        mut self,
        outcome: Result<(), EffectFailure<S, E>>,
    ) -> RestoreResult<O, N, S, E> {
        match outcome {
            Ok(()) => {
                self.attempts.push(Ok(()));
                Ok(XdpReplacementRollback {
                    retained: self.replacement.old,
                    cleanup: self.replacement.staged,
                    primary: self.primary,
                    restoration_attempts: self.attempts,
                })
            }
            Err(failure) => {
                self.attempts.push(Err(failure.cause));
                Err(XdpRestoreFailure {
                    restoration: XdpRestoration {
                        replacement: self.replacement,
                        switch: failure.remaining,
                        primary: self.primary,
                        attempts: self.attempts,
                    },
                })
            }
        }
    }
}

impl<O, N, S, E> XdpRestoreFailure<O, N, S, E> {
    /// Original forward failure; restoration failure never replaces it.
    pub fn primary(&self) -> &E {
        &self.restoration.primary
    }

    /// Every actual restoration attempt; blocked removals are not attempts.
    pub fn attempts(&self) -> &[Result<(), E>] {
        &self.restoration.attempts
    }

    /// Retain all ownership for a separate, caller-budgeted restoration pass.
    pub fn retry(self) -> XdpRestoration<O, N, S, E> {
        self.restoration
    }
}
