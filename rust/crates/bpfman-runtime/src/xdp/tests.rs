#![allow(clippy::expect_used)]

use super::*;
use std::{collections::BTreeSet, time::Duration};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    Prepare,
    Load,
    Directory,
    Program,
    Extension,
    Outer,
    Commit,
}

struct Receipt(XdpCleanupKind);

struct Fake {
    forward: Option<Stage>,
    fail: BTreeSet<XdpCleanupKind>,
    owned: BTreeSet<XdpCleanupKind>,
    attempts: Vec<XdpCleanupKind>,
    cancellation: Cancellation,
    cancel_at: Option<Stage>,
    committed: bool,
}

fn cause() -> LinkCause {
    Cause::Invalid("injected XDP effect failure").into()
}

fn key() -> XdpKey {
    XdpKey {
        nsid: NonZeroU64::MIN,
        ifindex: NonZeroU32::MIN,
    }
}

fn request() -> XdpAttach {
    XdpAttach {
        netns: Default::default(),
        program_id: NonZeroU32::MIN,
        interface: "eth0".parse().expect("interface"),
        priority: 50,
        proceed_on: Default::default(),
        metadata: Default::default(),
    }
}

impl Fake {
    fn new() -> Self {
        Self {
            forward: None,
            fail: Default::default(),
            owned: Default::default(),
            attempts: Vec::new(),
            cancellation: Cancellation::new(),
            cancel_at: None,
            committed: false,
        }
    }

    fn enter(&self, s: Stage) -> Result<(), LinkCause> {
        if self.cancel_at == Some(s) {
            self.cancellation.cancel();
        }
        if self.forward == Some(s) {
            Err(cause())
        } else {
            Ok(())
        }
    }

    fn acquire(
        &mut self,
        s: Stage,
        k: XdpCleanupKind,
    ) -> Result<Receipt, EffectFailure<Option<Receipt>, LinkCause>> {
        assert!(self.owned.insert(k), "duplicate acquisition");
        let receipt = Receipt(k);
        match self.enter(s) {
            Ok(()) => Ok(receipt),
            Err(cause) => Err(EffectFailure {
                cause,
                remaining: Some(receipt),
            }),
        }
    }

    fn remove(
        &mut self,
        r: Receipt,
        expected: XdpCleanupKind,
    ) -> Result<(), EffectFailure<Receipt, LinkCause>> {
        assert_eq!(r.0, expected);
        assert!(self.owned.contains(&expected));
        self.attempts.push(expected);
        if self.fail.contains(&expected) {
            return Err(EffectFailure {
                cause: cause(),
                remaining: r,
            });
        }
        self.owned.remove(&expected);
        Ok(())
    }
}

impl Effects for Fake {
    type Prepared = ();
    type Kernel = ();
    type Outer = Receipt;
    type Extension = Receipt;
    type Program = Receipt;
    type Directory = Receipt;
    type Record = Receipt;

    fn prepare(&mut self, _: &RuntimeWriter<'_>, _: &XdpAttach) -> Result<(XdpKey, ()), LinkCause> {
        self.enter(Stage::Prepare)?;
        Ok((key(), ()))
    }

    fn load(&mut self, _: &XdpAttach) -> Result<(), LinkCause> {
        self.enter(Stage::Load)
    }

    fn directory(
        &mut self,
        _: &RuntimeWriter<'_>,
        _: &(),
    ) -> Result<Receipt, EffectFailure<Option<Receipt>, LinkCause>> {
        self.acquire(Stage::Directory, XdpCleanupKind::Directory)
    }

    fn program(
        &mut self,
        _: &RuntimeWriter<'_>,
        _: &Receipt,
        _: &mut (),
    ) -> Result<Receipt, EffectFailure<Option<Receipt>, LinkCause>> {
        self.acquire(Stage::Program, XdpCleanupKind::Program)
    }

    fn extension(
        &mut self,
        _: &RuntimeWriter<'_>,
        _: &mut (),
        _: &Receipt,
        _: &(),
    ) -> Result<Receipt, EffectFailure<Option<Receipt>, LinkCause>> {
        self.acquire(Stage::Extension, XdpCleanupKind::Extension)
    }

    fn outer(
        &mut self,
        _: &RuntimeWriter<'_>,
        _: &(),
        _: &(),
    ) -> Result<Receipt, EffectFailure<Option<Receipt>, LinkCause>> {
        self.acquire(Stage::Outer, XdpCleanupKind::Outer)
    }

    fn commit(
        &mut self,
        _: &RuntimeWriter<'_>,
        r: &XdpAttach,
        key: XdpKey,
        _: &Receipt,
        _: &Receipt,
        _: &Receipt,
    ) -> Result<StoredLink, LinkCause> {
        self.enter(Stage::Commit)?;
        self.committed = true;
        Ok(StoredLink {
            id: NonZeroU64::MIN,
            program_id: r.program_id,
            details: bpfman_model::LinkDetails::Xdp(XdpLink {
                netns: Default::default(),
                slot: bpfman_model::XdpSlot::FIRST,
                key,
                interface: r.interface.clone(),
                priority: r.priority,
                proceed_on: r.proceed_on,
                dispatcher_id: NonZeroU32::MIN,
                revision: NonZeroU32::MIN,
            }),
            state: bpfman_model::LinkState::Attached {
                kernel_id: NonZeroU32::MIN,
            },
            pin_path: "pin".into(),
            metadata: Default::default(),
            created_at: "2026-10-05T00:00:00Z".into(),
        })
    }

    fn observe(
        &mut self,
        _: &RuntimeWriter<'_>,
        _: NonZeroU64,
    ) -> Result<Vec<ResourceFor<Self>>, LinkCause> {
        Err(cause())
    }

    fn remove_outer(
        &mut self,
        _: &RuntimeWriter<'_>,
        r: Receipt,
    ) -> Result<(), EffectFailure<Receipt, LinkCause>> {
        self.remove(r, XdpCleanupKind::Outer)
    }

    fn remove_extension(
        &mut self,
        _: &RuntimeWriter<'_>,
        r: Receipt,
    ) -> Result<(), EffectFailure<Receipt, LinkCause>> {
        self.remove(r, XdpCleanupKind::Extension)
    }

    fn remove_program(
        &mut self,
        _: &RuntimeWriter<'_>,
        r: Receipt,
    ) -> Result<(), EffectFailure<Receipt, LinkCause>> {
        self.remove(r, XdpCleanupKind::Program)
    }

    fn remove_directory(
        &mut self,
        _: &RuntimeWriter<'_>,
        r: Receipt,
    ) -> Result<(), EffectFailure<Receipt, LinkCause>> {
        self.remove(r, XdpCleanupKind::Directory)
    }

    fn remove_record(
        &mut self,
        _: &RuntimeWriter<'_>,
        r: Receipt,
    ) -> Result<(), EffectFailure<Receipt, LinkCause>> {
        self.remove(r, XdpCleanupKind::Record)
    }
}

fn writer(run: impl FnOnce(&RuntimeWriter<'_>)) {
    let temp = tempfile::tempdir().expect("tempdir");
    let layout = bpfman_fs::RuntimeLayout::try_from(temp.path().to_owned()).expect("layout");
    bpfman_fs::RuntimeDirectory::open_or_create(layout)
        .expect("runtime")
        .with_writer(
            AcquireOptions {
                timeout: Duration::from_secs(1),
                cancelled: None,
            },
            |w| run(&w),
        )
        .expect("writer");
}

#[test]
fn every_forward_failure_crosses_cleanup_failures_and_explicit_retry() {
    use XdpCleanupKind::*;
    for stage in [
        Stage::Prepare,
        Stage::Load,
        Stage::Directory,
        Stage::Program,
        Stage::Extension,
        Stage::Outer,
        Stage::Commit,
    ] {
        for failures in [
            vec![],
            vec![Outer],
            vec![Extension],
            vec![Program],
            vec![Extension, Program],
            vec![Directory],
        ] {
            writer(|w| {
                let mut f = Fake::new();
                f.forward = Some(stage);
                f.fail = failures.into_iter().collect();
                let c = f.cancellation.clone();
                let (_, report) =
                    attach(w, &mut f, &request(), &c, None).expect_err("forward failure");
                assert!(!f.committed);
                assert_eq!(report.unresolved(), f.owned.len());
                assert_eq!(
                    f.attempts.iter().copied().collect::<BTreeSet<_>>().len(),
                    f.attempts.len(),
                    "no inline retries"
                );
                if f.attempts.contains(&Outer) && f.fail.contains(&Outer) {
                    assert_eq!(f.attempts, [Outer]);
                }
                if f.owned.contains(&Extension) || f.owned.contains(&Program) {
                    assert!(!f.attempts.contains(&Directory));
                }
                if stage == Stage::Commit && f.fail.contains(&Extension) && !f.fail.contains(&Outer)
                {
                    assert!(
                        f.attempts.contains(&Program),
                        "independent cleanup still runs"
                    );
                }
                let previous = report.attempts().len();
                let unresolved = report.unresolved();
                f.fail.clear();
                let report = cleanup(w, &mut f, report.retry());
                assert_eq!(report.unresolved(), 0);
                assert!(f.owned.is_empty());
                assert_eq!(report.attempts().len(), previous + unresolved);
            });
        }
    }
}

#[test]
fn cancellation_uses_compensation_until_commit_wins() {
    for stage in [
        Stage::Prepare,
        Stage::Load,
        Stage::Directory,
        Stage::Program,
        Stage::Extension,
        Stage::Outer,
        Stage::Commit,
    ] {
        writer(|w| {
            let mut f = Fake::new();
            f.cancel_at = Some(stage);
            let c = f.cancellation.clone();
            let result = attach(w, &mut f, &request(), &c, None);
            if stage == Stage::Commit {
                assert!(result.is_ok());
                assert!(f.committed);
                assert!(f.attempts.is_empty());
            } else {
                let (cause, report) = result.expect_err("cancelled");
                assert_eq!(cause.kind(), crate::LinkErrorKind::Cancelled);
                assert_eq!(report.unresolved(), 0);
                assert!(f.owned.is_empty());
            }
        });
    }
}

#[test]
fn last_detach_blocks_record_deletion_until_artifacts_are_gone() {
    use XdpCleanupKind::*;
    for failed in [Outer, Extension, Program, Directory, Record] {
        writer(|w| {
            let mut f = Fake::new();
            f.owned = [Outer, Extension, Program, Directory, Record]
                .into_iter()
                .collect();
            f.fail.insert(failed);
            let resources = vec![
                Resource::Record(Receipt(Record)),
                Resource::Directory(Receipt(Directory)),
                Resource::Program(Receipt(Program)),
                Resource::Extension(Receipt(Extension)),
                Resource::Outer(Receipt(Outer)),
            ];
            let report = cleanup(w, &mut f, XdpCleanup::new(resources));
            assert_eq!(report.unresolved(), f.owned.len());
            if failed != Record {
                assert!(!f.attempts.contains(&Record));
            }
            f.fail.clear();
            let report = cleanup(w, &mut f, report.retry());
            assert_eq!(report.unresolved(), 0);
            assert!(f.owned.is_empty());
        });
    }
}
