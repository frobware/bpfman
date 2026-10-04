//! Stateful fault injection through the production forward/cleanup interpreter.
#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use bpfman_core::LinkCleanupKind;
use bpfman_model::{LinkDetails, LinkState};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum Point {
    Prepare,
    Create,
    Attach,
    Pin,
    ObserveNewPin,
    Finalise,
    Observe,
    ObservePin,
    Release,
    Unpin,
    Delete,
}

struct Live;
struct Pin;
struct Receipt;

struct Fake {
    fail: BTreeSet<Point>,
    cancel: Option<Point>,
    cancellation: Cancellation,
    calls: Vec<Point>,
    record: Option<StoredLink>,
    live: bool,
    pinned: bool,
}

impl Fake {
    fn new() -> Self {
        Self {
            fail: BTreeSet::new(),
            cancel: None,
            cancellation: Cancellation::new(),
            calls: Vec::new(),
            record: None,
            live: false,
            pinned: false,
        }
    }

    fn hit(&mut self, point: Point) -> Result<(), LinkCause> {
        self.calls.push(point);
        if self.cancel == Some(point) {
            self.cancellation.cancel();
        }
        if self.fail.contains(&point) {
            return Err(bpfman_store::Error::new(
                bpfman_store::ErrorKind::Unavailable,
                std::io::Error::other(format!("injected {point:?}")),
            )
            .into());
        }
        Ok(())
    }
}

fn request() -> TracepointAttach {
    TracepointAttach {
        program_id: NonZeroU32::new(42).expect("id"),
        target: "sched/sched_switch".parse().expect("target"),
        metadata: Default::default(),
    }
}

impl Effects for Fake {
    type Prepared = ();
    type Live = Live;
    type Pin = Pin;
    type Receipt = Receipt;

    fn prepare(&mut self, _: &RuntimeWriter<'_>, _: NonZeroU32) -> Result<(), LinkCause> {
        self.hit(Point::Prepare)
    }

    fn create(
        &mut self,
        writer: &RuntimeWriter<'_>,
        request: &TracepointAttach,
        created: &str,
    ) -> Result<(StoredLink, Receipt), LinkCause> {
        self.hit(Point::Create)?;
        assert!(self.record.is_none());
        let id = NonZeroU64::MIN;
        let record = StoredLink {
            id,
            program_id: request.program_id,
            details: LinkDetails::Tracepoint(request.target.clone()),
            state: LinkState::Pending,
            metadata: request.metadata.clone(),
            created_at: created.into(),
            pin_path: writer
                .layout()
                .link_pin_path(id)
                .to_str()
                .expect("path")
                .into(),
        };
        self.record = Some(record.clone());
        Ok((record, Receipt))
    }

    fn attach(
        &mut self,
        _: &RuntimeWriter<'_>,
        _: (),
        _: &TracepointAttach,
    ) -> Result<Live, LinkCause> {
        self.hit(Point::Attach)?;
        assert!(self.record.is_some());
        assert!(!self.live && !self.pinned);
        self.live = true;
        Ok(Live)
    }

    fn pin(
        &mut self,
        _: &RuntimeWriter<'_>,
        _: Live,
        _: NonZeroU64,
    ) -> Result<Pin, EffectFailure<Option<Pin>, LinkCause>> {
        assert!(self.live && !self.pinned);
        // This consuming adapter closes a transient link on pre-pin failure.
        self.live = false;
        self.hit(Point::Pin).map_err(|cause| EffectFailure {
            cause,
            remaining: None,
        })?;
        self.pinned = true;
        self.hit(Point::ObserveNewPin)
            .map_err(|cause| EffectFailure {
                cause,
                remaining: Some(Pin),
            })?;
        Ok(Pin)
    }

    fn finalise(
        &mut self,
        _: &RuntimeWriter<'_>,
        receipt: Receipt,
        _: &Pin,
    ) -> Result<StoredLink, EffectFailure<Receipt, LinkCause>> {
        self.hit(Point::Finalise).map_err(|cause| EffectFailure {
            cause,
            remaining: receipt,
        })?;
        assert!(self.pinned);
        let record = self.record.as_mut().expect("pending intent");
        assert_eq!(record.state, LinkState::Pending);
        record.state = LinkState::Attached {
            kernel_id: NonZeroU32::new(700).expect("id"),
        };
        Ok(record.clone())
    }

    fn observe(
        &mut self,
        _: &RuntimeWriter<'_>,
        _: NonZeroU64,
    ) -> Result<(StoredLink, Receipt), LinkCause> {
        self.hit(Point::Observe)?;
        Ok((self.record.clone().expect("record"), Receipt))
    }

    fn observe_pin(
        &mut self,
        _: &RuntimeWriter<'_>,
        _: &StoredLink,
    ) -> Result<Option<Pin>, LinkCause> {
        self.hit(Point::ObservePin)?;
        Ok(self.pinned.then_some(Pin))
    }

    fn release(
        &mut self,
        _: &RuntimeWriter<'_>,
        receipt: Live,
    ) -> Result<(), EffectFailure<Live, LinkCause>> {
        self.hit(Point::Release).map_err(|cause| EffectFailure {
            cause,
            remaining: receipt,
        })?;
        assert!(self.live);
        self.live = false;
        Ok(())
    }

    fn unpin(
        &mut self,
        _: &RuntimeWriter<'_>,
        receipt: Pin,
    ) -> Result<(), EffectFailure<Pin, LinkCause>> {
        self.hit(Point::Unpin).map_err(|cause| EffectFailure {
            cause,
            remaining: receipt,
        })?;
        assert!(self.pinned);
        self.pinned = false;
        Ok(())
    }

    fn delete(
        &mut self,
        _: &RuntimeWriter<'_>,
        receipt: Receipt,
    ) -> Result<(), EffectFailure<Receipt, LinkCause>> {
        self.hit(Point::Delete).map_err(|cause| EffectFailure {
            cause,
            remaining: receipt,
        })?;
        assert!(
            !self.live && !self.pinned,
            "record deletion must follow attachment release"
        );
        assert!(self.record.take().is_some());
        Ok(())
    }
}

fn scope(test: impl FnOnce(&RuntimeWriter<'_>)) {
    let temporary = tempfile::tempdir().expect("tempdir");
    let runtime = bpfman_fs::RuntimeDirectory::open_or_create(
        bpfman_fs::RuntimeLayout::try_from(temporary.path().to_owned()).expect("layout"),
    )
    .expect("runtime");
    runtime
        .with_writer(
            AcquireOptions {
                timeout: std::time::Duration::from_secs(1),
                cancelled: None,
            },
            |writer| test(&writer),
        )
        .expect("writer");
}

#[test]
fn every_forward_failure_uses_production_compensation_with_owned_partial_results() {
    for forward in [
        Point::Prepare,
        Point::Create,
        Point::Attach,
        Point::Pin,
        Point::ObserveNewPin,
        Point::Finalise,
    ] {
        for rollback in [None, Some(Point::Unpin), Some(Point::Delete)] {
            scope(|writer| {
                let mut effects = Fake::new();
                effects.fail.insert(forward);
                effects.fail.extend(rollback);
                let token = effects.cancellation.clone();

                let failure = attach(writer, &mut effects, &request(), "now", &token)
                    .expect_err("forward fails");
                match failure {
                    Failed::Before(_) => {
                        assert!(matches!(forward, Point::Prepare | Point::Create));
                        assert!(effects.record.is_none());
                    }
                    Failed::After { report, .. } => {
                        let has_pin = matches!(forward, Point::ObserveNewPin | Point::Finalise);
                        let blocked = has_pin && rollback == Some(Point::Unpin);
                        assert_eq!(effects.pinned, blocked);
                        assert_eq!(
                            effects.record.is_some(),
                            blocked || rollback == Some(Point::Delete)
                        );
                        assert_eq!(
                            report.attempts().len(),
                            if blocked { 1 } else { usize::from(has_pin) + 1 }
                        );
                        if blocked {
                            assert_eq!(report.unresolved(), 2);
                            assert!(!effects.calls.contains(&Point::Delete));
                        }

                        // An unchanged obstruction still fails. A new pass is
                        // caller policy, not an assumption that errors are transient.
                        let unresolved = report.unresolved();
                        let history = report.attempts().len();
                        let report = cleanup(writer, &mut effects, report.retry());
                        assert_eq!(report.unresolved(), unresolved);
                        assert_eq!(
                            report.attempts().len(),
                            history + usize::from(unresolved > 0)
                        );

                        let history = report.attempts().len();
                        effects.fail.clear();
                        let report = cleanup(writer, &mut effects, report.retry());

                        assert_eq!(report.unresolved(), 0);
                        assert!(report.attempts().len() >= history);
                        assert!(!effects.live && !effects.pinned && effects.record.is_none());
                    }
                }
            });
        }
    }
}

#[test]
fn cancellation_releases_transient_and_pinned_links_but_never_undoes_finalisation() {
    for point in [
        Point::Prepare,
        Point::Create,
        Point::Attach,
        Point::Pin,
        Point::ObserveNewPin,
        Point::Finalise,
    ] {
        scope(|writer| {
            let mut effects = Fake::new();
            effects.cancel = Some(point);
            let token = effects.cancellation.clone();

            let result = attach(writer, &mut effects, &request(), "now", &token);

            assert!(token.is_cancelled());
            if point == Point::Finalise {
                assert!(result.is_ok());
                assert!(effects.pinned && effects.record.is_some());
                assert!(!effects.calls.contains(&Point::Unpin));
            } else {
                match result.expect_err("cancelled") {
                    Failed::Before(cause) | Failed::After { primary: cause, .. } => {
                        assert_eq!(cause.kind(), crate::LinkErrorKind::Cancelled)
                    }
                }
                assert!(!effects.live && !effects.pinned && effects.record.is_none());
                if point == Point::Attach {
                    assert!(effects.calls.contains(&Point::Release));
                }
            }
        });
    }
}

#[test]
fn failed_live_release_blocks_record_deletion_and_retry_preserves_history() {
    scope(|writer| {
        let mut effects = Fake::new();
        effects.cancel = Some(Point::Attach);
        effects.fail.insert(Point::Release);
        let token = effects.cancellation.clone();

        let Failed::After { report, .. } =
            attach(writer, &mut effects, &request(), "now", &token).expect_err("cancelled")
        else {
            panic!("expected retained work");
        };

        assert_eq!(report.unresolved(), 2);
        assert_eq!(report.attempts().len(), 1);
        assert_eq!(report.attempts()[0].kind, LinkCleanupKind::ReleaseLive);
        assert!(effects.live && effects.record.is_some());
        assert!(!effects.calls.contains(&Point::Delete));

        effects.fail.clear();
        let report = cleanup(writer, &mut effects, report.retry());

        assert_eq!(report.unresolved(), 0);
        assert_eq!(report.attempts().len(), 3);
        assert!(report.attempts()[0].outcome.is_err());
        assert!(
            report.attempts()[1..]
                .iter()
                .all(|attempt| attempt.outcome.is_ok())
        );
    });
}

#[test]
fn detach_cancels_preflight_but_shields_destructive_effects_and_retries_only_residue() {
    for point in [
        Point::Observe,
        Point::ObservePin,
        Point::Unpin,
        Point::Delete,
    ] {
        scope(|writer| {
            let mut effects = Fake::new();
            let token = effects.cancellation.clone();
            let record = match attach(writer, &mut effects, &request(), "now", &token) {
                Ok(record) => record,
                Err(_) => panic!("attach"),
            };
            effects.cancel = Some(point);

            let result = detach(writer, &mut effects, record.id, &token);

            if matches!(point, Point::Observe | Point::ObservePin) {
                assert_eq!(
                    result.err().expect("cancelled").kind(),
                    crate::LinkErrorKind::Cancelled
                );
                assert!(effects.pinned && effects.record.is_some());
            } else {
                let report = result.expect("finish teardown");
                assert_eq!(report.unresolved(), 0);
                assert_eq!(report.attempts().len(), 2);
                assert!(!effects.pinned && effects.record.is_none());
            }
        });
    }

    scope(|writer| {
        let mut effects = Fake::new();
        let token = effects.cancellation.clone();
        let record = match attach(writer, &mut effects, &request(), "now", &token) {
            Ok(record) => record,
            Err(_) => panic!("attach"),
        };
        effects.fail.insert(Point::Delete);
        let report = detach(writer, &mut effects, record.id, &token).expect("teardown report");

        assert_eq!(report.unresolved(), 1);
        assert!(!effects.pinned && effects.record.is_some());

        effects.fail.clear();
        let report = cleanup(writer, &mut effects, report.retry());

        assert_eq!(report.unresolved(), 0);
        assert_eq!(
            effects.calls.iter().filter(|p| **p == Point::Unpin).count(),
            1
        );
        assert_eq!(report.attempts().len(), 3);
    });
}
