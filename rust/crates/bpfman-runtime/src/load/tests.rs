//! Faults enter the production forward interpreter, not a test-only load plan.
#![allow(clippy::expect_used)]
use super::*;
use crate::{
    LoadCleanup,
    load_error::{Failure, retry},
};
use bpfman_core::{CompensationKind, EffectFailure};
use bpfman_model::ProgramType;
use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    num::NonZeroU32,
    path::PathBuf,
    rc::Rc,
};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Resource {
    Program,
    Directory,
    Map(u8),
    Staging,
    Bytecode,
    Unrelated,
    SharedMap,
}
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Event {
    OpenStore,
    Prepare,
    LoadKernel,
    PinProgram,
    CreateDirectory,
    PinMap(u8),
    Publish,
    Persist,
    RemoveBytecode,
    RemoveProgram,
    RemoveMap(u8),
    RemoveDirectory,
}
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Fault {
    Before(Event),
    AfterAcquisition(Event),
    StagedPublication,
    BeforeCommit,
    WrongRuntime(Event),
}
#[derive(Default, Debug)]
struct Counts {
    receipts: Cell<usize>,
    dropped_receipts: RefCell<Vec<usize>>,
    errors: Cell<usize>,
    dropped_errors: RefCell<Vec<usize>>,
    handles: Cell<usize>,
}
#[derive(Debug, thiserror::Error)]
#[error("injected {fault:?}")]
struct TestError {
    fault: Fault,
    serial: usize,
    counts: Rc<Counts>,
}
impl Drop for TestError {
    fn drop(&mut self) {
        self.counts.dropped_errors.borrow_mut().push(self.serial);
    }
}
// Deliberately no Clone for any ownership or error type.
struct Receipt {
    resource: Resource,
    owner: Rc<()>,
    serial: usize,
    counts: Rc<Counts>,
}
impl Drop for Receipt {
    fn drop(&mut self) {
        self.counts.dropped_receipts.borrow_mut().push(self.serial);
    }
}
struct Program(Receipt);
struct Map(Receipt);
struct BytecodeReceipt(Receipt);
struct Directory(Receipt);
struct Kernel(Rc<Counts>);
impl Drop for Kernel {
    fn drop(&mut self) {
        self.0.handles.set(self.0.handles.get() - 1);
    }
}

struct Fake {
    runtime: PathBuf,
    owner: Rc<()>,
    counts: Rc<Counts>,
    resources: BTreeSet<Resource>,
    records: BTreeSet<u32>,
    map_sets: BTreeSet<u32>,
    faults: BTreeSet<Fault>,
    events: Vec<Event>,
}
fn baseline() -> BTreeSet<Resource> {
    BTreeSet::from([Resource::Unrelated, Resource::SharedMap])
}
fn id() -> NonZeroU32 {
    NonZeroU32::new(42).expect("fixture")
}
impl Fake {
    fn new(writer: &RuntimeWriter<'_>, faults: impl IntoIterator<Item = Fault>) -> Self {
        Self {
            runtime: writer.database_path(),
            owner: Rc::new(()),
            counts: Rc::default(),
            resources: baseline(),
            records: BTreeSet::from([7]),
            map_sets: BTreeSet::from([7]),
            faults: faults.into_iter().collect(),
            events: vec![],
        }
    }
    fn error(&self, fault: Fault) -> TestError {
        let serial = self.counts.errors.get();
        self.counts.errors.set(serial + 1);
        TestError {
            fault,
            serial,
            counts: self.counts.clone(),
        }
    }
    fn check(&self, fault: Fault) -> Result<(), TestError> {
        if self.faults.contains(&fault) {
            Err(self.error(fault))
        } else {
            Ok(())
        }
    }
    fn enter(&mut self, writer: &RuntimeWriter<'_>, event: Event) -> Result<(), TestError> {
        self.events.push(event);
        if writer.database_path() != self.runtime {
            return Err(self.error(Fault::WrongRuntime(event)));
        }
        self.check(Fault::Before(event))
    }
    fn acquire(&mut self, resource: Resource) -> Receipt {
        assert!(
            self.resources.insert(resource),
            "duplicate acquisition {resource:?}"
        );
        let serial = self.counts.receipts.get();
        self.counts.receipts.set(serial + 1);
        Receipt {
            resource,
            owner: self.owner.clone(),
            serial,
            counts: self.counts.clone(),
        }
    }
    fn pin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        event: Event,
        resource: Resource,
    ) -> Result<Receipt, EffectFailure<Option<Receipt>, TestError>> {
        self.enter(writer, event).map_err(|cause| EffectFailure {
            cause,
            remaining: None,
        })?;
        let receipt = self.acquire(resource);
        match self.check(Fault::AfterAcquisition(event)) {
            Ok(()) => Ok(receipt),
            Err(cause) => Err(EffectFailure {
                cause,
                remaining: Some(receipt),
            }),
        }
    }
    fn remove(
        &mut self,
        writer: &RuntimeWriter<'_>,
        event: Event,
        receipt: &Receipt,
    ) -> Result<(), TestError> {
        assert!(!self.records.contains(&42), "cleanup after commit");
        assert!(Rc::ptr_eq(&receipt.owner, &self.owner), "foreign receipt");
        self.enter(writer, event)?;
        if event == Event::RemoveDirectory {
            assert!(
                !self.resources.iter().any(|r| matches!(r, Resource::Map(_))),
                "directory removal while map pins remain"
            );
        }
        assert!(
            self.resources.remove(&receipt.resource),
            "double removal or unowned resource"
        );
        Ok(())
    }
    fn assert_clean(&self) {
        assert_eq!(self.resources, baseline());
        assert_eq!(self.records, BTreeSet::from([7]));
        assert_eq!(self.map_sets, BTreeSet::from([7]));
        assert_eq!(self.counts.handles.get(), 0);
    }
}
fn wrap_partial<T>(
    failure: EffectFailure<Option<Receipt>, TestError>,
    wrap: impl FnOnce(Receipt) -> T,
) -> EffectFailure<Option<T>, TestError> {
    EffectFailure {
        cause: failure.cause,
        remaining: failure.remaining.map(wrap),
    }
}
impl LoadCleanup for Fake {
    type ProgramPin = Program;
    type MapPin = Map;
    type Bytecode = BytecodeReceipt;
    type Error = TestError;
    fn remove_bytecode(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: BytecodeReceipt,
    ) -> Result<(), EffectFailure<BytecodeReceipt, TestError>> {
        self.remove(writer, Event::RemoveBytecode, &receipt.0)
            .map_err(|cause| EffectFailure {
                cause,
                remaining: receipt,
            })
    }
    fn remove_program_pin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Program,
    ) -> Result<(), EffectFailure<Program, TestError>> {
        self.remove(writer, Event::RemoveProgram, &receipt.0)
            .map_err(|cause| EffectFailure {
                cause,
                remaining: receipt,
            })
    }
    fn remove_map_pin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Map,
    ) -> Result<(), EffectFailure<Map, TestError>> {
        let Resource::Map(n) = receipt.0.resource else {
            unreachable!("map receipt")
        };
        self.remove(writer, Event::RemoveMap(n), &receipt.0)
            .map_err(|cause| EffectFailure {
                cause,
                remaining: receipt,
            })
    }
}
impl LoadEffects for Fake {
    type Store = ();
    type Prepared = ();
    type Kernel = Kernel;
    fn open_store(&mut self, writer: &RuntimeWriter<'_>) -> Result<(), TestError> {
        self.enter(writer, Event::OpenStore)
    }
    fn prepare(&mut self, writer: &RuntimeWriter<'_>) -> Result<(), TestError> {
        self.enter(writer, Event::Prepare)
    }
    fn load_kernel(
        &mut self,
        writer: &RuntimeWriter<'_>,
        input: &Inputs<'_>,
    ) -> Result<Kernel, TestError> {
        self.enter(writer, Event::LoadKernel)?;
        assert_eq!(input.name.as_str(), "trace");
        self.counts.handles.set(self.counts.handles.get() + 1);
        let kernel = Kernel(self.counts.clone());
        self.check(Fault::AfterAcquisition(Event::LoadKernel))?;
        Ok(kernel)
    }
    fn pin_program(
        &mut self,
        writer: &RuntimeWriter<'_>,
        _: &(),
        _: &mut Kernel,
        _: &Symbol,
    ) -> Result<Program, EffectFailure<Option<Program>, TestError>> {
        assert_eq!(self.counts.handles.get(), 1);
        self.pin(writer, Event::PinProgram, Resource::Program)
            .map(Program)
            .map_err(|f| wrap_partial(f, Program))
    }
    fn program_id(_: &Program) -> NonZeroU32 {
        id()
    }
    fn create_map_directory(
        &mut self,
        writer: &RuntimeWriter<'_>,
        _: &(),
        program: NonZeroU32,
    ) -> Result<Directory, EffectFailure<Option<Directory>, TestError>> {
        assert_eq!(program, id());
        assert!(self.resources.contains(&Resource::Program));
        self.pin(writer, Event::CreateDirectory, Resource::Directory)
            .map(Directory)
            .map_err(|f| wrap_partial(f, Directory))
    }
    fn pin_map(
        &mut self,
        writer: &RuntimeWriter<'_>,
        _: &Kernel,
        directory: &Directory,
        name: &str,
    ) -> Result<Map, EffectFailure<Option<Map>, TestError>> {
        assert!(self.resources.contains(&directory.0.resource));
        let n = name.parse::<u8>().expect("map fixture name");
        self.pin(writer, Event::PinMap(n), Resource::Map(n))
            .map(Map)
            .map_err(|f| wrap_partial(f, Map))
    }
    fn publish(
        &mut self,
        writer: &RuntimeWriter<'_>,
        program: NonZeroU32,
        input: &Inputs<'_>,
    ) -> Result<BytecodeReceipt, EffectFailure<Vec<BytecodeReceipt>, TestError>> {
        assert_eq!(program, id());
        assert!(self.resources.contains(&Resource::Program));
        for name in &input.object.maps {
            assert!(
                self.resources
                    .contains(&Resource::Map(name.parse().expect("map id")))
            );
        }
        self.enter(writer, Event::Publish)
            .map_err(|cause| EffectFailure {
                cause,
                remaining: vec![],
            })?;
        let mut receipt = BytecodeReceipt(self.acquire(Resource::Staging));
        if let Err(cause) = self.check(Fault::StagedPublication) {
            return Err(EffectFailure {
                cause,
                remaining: vec![receipt],
            });
        }
        assert!(self.resources.remove(&Resource::Staging));
        assert!(self.resources.insert(Resource::Bytecode));
        receipt.0.resource = Resource::Bytecode;
        match self.check(Fault::AfterAcquisition(Event::Publish)) {
            Ok(()) => Ok(receipt),
            Err(cause) => Err(EffectFailure {
                cause,
                remaining: vec![receipt],
            }),
        }
    }
    fn persist(
        &mut self,
        writer: &RuntimeWriter<'_>,
        program: NonZeroU32,
        input: &Inputs<'_>,
    ) -> Result<StoredProgramSummary, TestError> {
        self.enter(writer, Event::Persist)?;
        assert!(self.resources.contains(&Resource::Bytecode));
        // Build both transaction-local rows, but expose neither until commit.
        let mut next_maps = self.map_sets.clone();
        let mut next_records = self.records.clone();
        assert!(next_maps.insert(program.get()));
        assert!(next_records.insert(program.get()));
        self.check(Fault::BeforeCommit)?;
        self.map_sets = next_maps;
        self.records = next_records;
        Ok(StoredProgramSummary::new(
            program,
            input.name.as_str().into(),
            ProgramType::Tracepoint,
            input.metadata.clone(),
            vec![],
        ))
    }
}
impl super::CleanupEffects for Fake {
    type MapDirectory = Directory;
    fn remove_map_directory(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Directory,
    ) -> Result<(), EffectFailure<Directory, TestError>> {
        self.remove(writer, Event::RemoveDirectory, &receipt.0)
            .map_err(|cause| EffectFailure {
                cause,
                remaining: receipt,
            })
    }
}

fn with_writer(test: impl FnOnce(&RuntimeWriter<'_>)) {
    let temp = tempfile::tempdir().expect("temporary runtime");
    let runtime = RuntimeDirectory::open_or_create(
        RuntimeLayout::try_from(temp.path().to_owned()).expect("layout"),
    )
    .expect("runtime");
    runtime
        .with_writer(
            AcquireOptions {
                timeout: Duration::from_secs(1),
                cancelled: None,
            },
            |writer| test(&writer),
        )
        .expect("writer");
}
fn invoke<F: LoadEffects>(
    writer: &RuntimeWriter<'_>,
    fake: &mut F,
    maps: &[&str],
) -> Result<StoredProgramSummary, FailureFor<F>> {
    // Inputs are already validated at the public boundary. Nothing here
    // constructs policy transitions: run() is exactly the CLI's interpreter.
    run(
        writer,
        fake,
        &Inputs {
            object: &LocalObject {
                bytes: b"ELF snapshot".to_vec(),
                license: "GPL".into(),
                maps: maps.iter().map(|s| (*s).into()).collect(),
            },
            source: "original.o",
            name: &Symbol::try_from("trace").expect("symbol"),
            metadata: &BTreeMap::from([("bpfman.io/application".into(), "fault-test".into())]),
            created_at: "2026-10-03T00:00:00Z",
        },
    )
}
fn failed(result: Result<StoredProgramSummary, FailureFor<Fake>>) -> FailureFor<Fake> {
    match result {
        Err(failure) => failure,
        Ok(_) => unreachable!("injected load must fail"),
    }
}
fn primary(failure: &FailureFor<Fake>) -> &TestError {
    match failure {
        Failure::NoOwnedArtifacts(error) => error,
        Failure::Compensated { report, .. } => report.primary(),
    }
}
fn unresolved(failure: &FailureFor<Fake>) -> BTreeSet<Resource> {
    use bpfman_core::LoadCompensation;
    match failure {
        Failure::NoOwnedArtifacts(_) => BTreeSet::new(),
        Failure::Compensated {
            report, directory, ..
        } => {
            let mut resources: BTreeSet<_> = report
                .remaining()
                .iter()
                .map(|pending| match pending.instruction() {
                    LoadCompensation::RemoveProgramPin(r) => r.0.resource,
                    LoadCompensation::RemoveMapPin(r) => r.0.resource,
                    LoadCompensation::RemoveBytecode(r) => r.0.resource,
                })
                .collect();
            if directory.is_some() {
                resources.insert(Resource::Directory);
            }
            resources
        }
    }
}
fn history(failure: &FailureFor<Fake>) -> Vec<(usize, CompensationKind, Option<Fault>)> {
    match failure {
        Failure::NoOwnedArtifacts(_) => vec![],
        Failure::Compensated { report, .. } => report
            .attempts()
            .iter()
            .map(|a| (a.id, a.kind, a.outcome.as_ref().err().map(|e| e.fault)))
            .collect(),
    }
}
fn assert_dropped_once(counts: &Counts) {
    let mut receipts = counts.dropped_receipts.borrow().clone();
    receipts.sort_unstable();
    let mut errors = counts.dropped_errors.borrow().clone();
    errors.sort_unstable();
    assert_eq!(receipts, (0..counts.receipts.get()).collect::<Vec<_>>());
    assert_eq!(errors, (0..counts.errors.get()).collect::<Vec<_>>());
}

const FORWARD: &[Event] = &[
    Event::OpenStore,
    Event::Prepare,
    Event::LoadKernel,
    Event::PinProgram,
    Event::CreateDirectory,
    Event::PinMap(1),
    Event::PinMap(2),
    Event::PinMap(3),
    Event::Publish,
    Event::Persist,
];
const CLEANUP: &[Event] = &[
    Event::RemoveBytecode,
    Event::RemoveProgram,
    Event::RemoveMap(3),
    Event::RemoveMap(2),
    Event::RemoveMap(1),
    Event::RemoveDirectory,
];
struct Case {
    fault: Fault,
    last: Event,
    owned: Vec<Resource>,
}
fn cases() -> Vec<Case> {
    use Event::*;
    use Fault::*;
    use Resource as R;
    vec![
        Case {
            fault: Before(OpenStore),
            last: OpenStore,
            owned: vec![],
        },
        Case {
            fault: Before(Prepare),
            last: Prepare,
            owned: vec![],
        },
        Case {
            fault: Before(LoadKernel),
            last: LoadKernel,
            owned: vec![],
        },
        Case {
            fault: AfterAcquisition(LoadKernel),
            last: LoadKernel,
            owned: vec![],
        },
        Case {
            fault: Before(PinProgram),
            last: PinProgram,
            owned: vec![],
        },
        Case {
            fault: AfterAcquisition(PinProgram),
            last: PinProgram,
            owned: vec![R::Program],
        },
        Case {
            fault: Before(CreateDirectory),
            last: CreateDirectory,
            owned: vec![R::Program],
        },
        Case {
            fault: AfterAcquisition(CreateDirectory),
            last: CreateDirectory,
            owned: vec![R::Program, R::Directory],
        },
        Case {
            fault: Before(PinMap(1)),
            last: PinMap(1),
            owned: vec![R::Program, R::Directory],
        },
        Case {
            fault: AfterAcquisition(PinMap(1)),
            last: PinMap(1),
            owned: vec![R::Program, R::Directory, R::Map(1)],
        },
        Case {
            fault: Before(PinMap(2)),
            last: PinMap(2),
            owned: vec![R::Program, R::Directory, R::Map(1)],
        },
        Case {
            fault: AfterAcquisition(PinMap(2)),
            last: PinMap(2),
            owned: vec![R::Program, R::Directory, R::Map(1), R::Map(2)],
        },
        Case {
            fault: Before(PinMap(3)),
            last: PinMap(3),
            owned: vec![R::Program, R::Directory, R::Map(1), R::Map(2)],
        },
        Case {
            fault: AfterAcquisition(PinMap(3)),
            last: PinMap(3),
            owned: vec![R::Program, R::Directory, R::Map(1), R::Map(2), R::Map(3)],
        },
        Case {
            fault: Before(Publish),
            last: Publish,
            owned: vec![R::Program, R::Directory, R::Map(1), R::Map(2), R::Map(3)],
        },
        Case {
            fault: StagedPublication,
            last: Publish,
            owned: vec![
                R::Program,
                R::Directory,
                R::Map(1),
                R::Map(2),
                R::Map(3),
                R::Staging,
            ],
        },
        Case {
            fault: AfterAcquisition(Publish),
            last: Publish,
            owned: vec![
                R::Program,
                R::Directory,
                R::Map(1),
                R::Map(2),
                R::Map(3),
                R::Bytecode,
            ],
        },
        Case {
            fault: Before(Persist),
            last: Persist,
            owned: vec![
                R::Program,
                R::Directory,
                R::Map(1),
                R::Map(2),
                R::Map(3),
                R::Bytecode,
            ],
        },
        Case {
            fault: BeforeCommit,
            last: Persist,
            owned: vec![
                R::Program,
                R::Directory,
                R::Map(1),
                R::Map(2),
                R::Map(3),
                R::Bytecode,
            ],
        },
    ]
}
fn removal(resource: Resource) -> Event {
    match resource {
        Resource::Program => Event::RemoveProgram,
        Resource::Directory => Event::RemoveDirectory,
        Resource::Map(n) => Event::RemoveMap(n),
        Resource::Staging | Resource::Bytecode => Event::RemoveBytecode,
        _ => unreachable!("no receipt for shared/unrelated resources"),
    }
}

#[test]
fn every_forward_failure_crossed_with_every_cleanup_failure_subset() {
    with_writer(|writer| {
        let mut scenarios = 0;
        for case in cases() {
            for mask in 0..(1 << CLEANUP.len()) {
                let injected: BTreeSet<_> = CLEANUP
                    .iter()
                    .enumerate()
                    .filter(|(n, _)| mask & (1 << n) != 0)
                    .map(|(_, e)| Fault::Before(*e))
                    .chain([case.fault])
                    .collect();
                let mut fake = Fake::new(writer, injected.clone());
                let failure = failed(invoke(writer, &mut fake, &["1", "2", "3"]));
                assert_eq!(primary(&failure).fault, case.fault);
                assert_eq!(
                    fake.counts.handles.get(),
                    0,
                    "kernel handles leaked at {:?}",
                    case.fault
                );
                let last = FORWARD
                    .iter()
                    .position(|e| *e == case.last)
                    .expect("forward event");
                assert_eq!(&fake.events[..=last], &FORWARD[..=last]);
                let blocked = case.owned.iter().any(|r| {
                    matches!(r, Resource::Map(_)) && injected.contains(&Fault::Before(removal(*r)))
                });
                let expected: Vec<_> = CLEANUP
                    .iter()
                    .copied()
                    .filter(|e| {
                        case.owned.iter().any(|r| removal(*r) == *e)
                            && !(*e == Event::RemoveDirectory && blocked)
                    })
                    .collect();
                assert_eq!(
                    &fake.events[last + 1..],
                    expected,
                    "case {:?} cleanup mask {mask}",
                    case.fault
                );
                let remaining: BTreeSet<_> = case
                    .owned
                    .iter()
                    .copied()
                    .filter(|r| {
                        injected.contains(&Fault::Before(removal(*r)))
                            || (*r == Resource::Directory && blocked)
                    })
                    .collect();
                assert_eq!(unresolved(&failure), remaining);
                assert_eq!(
                    fake.resources,
                    baseline().union(&remaining).copied().collect()
                );
                assert_eq!(fake.records, BTreeSet::from([7]));
                assert_eq!(fake.map_sets, BTreeSet::from([7]));
                let before = history(&failure);
                let independent: Vec<_> = expected
                    .iter()
                    .filter(|e| **e != Event::RemoveDirectory)
                    .collect();
                assert_eq!(before.len(), independent.len());
                for (attempt, event) in before.iter().zip(independent) {
                    assert_eq!(
                        attempt.2.is_some(),
                        injected.contains(&Fault::Before(*event))
                    );
                }
                if let Failure::Compensated {
                    directory_attempts, ..
                } = &failure
                {
                    assert_eq!(
                        directory_attempts.len(),
                        usize::from(expected.contains(&Event::RemoveDirectory))
                    );
                }
                assert!(
                    fake.counts.dropped_errors.borrow().is_empty(),
                    "error history was discarded"
                );
                let original_serial = primary(&failure).serial;
                fake.events.clear();
                fake.faults.clear();
                let failure = retry(writer, &mut fake, failure);
                let expected_retry: Vec<_> = CLEANUP
                    .iter()
                    .copied()
                    .filter(|e| remaining.iter().any(|r| removal(*r) == *e))
                    .collect();
                assert_eq!(
                    fake.events, expected_retry,
                    "retry must contain only unresolved work"
                );
                assert!(unresolved(&failure).is_empty());
                let after = history(&failure);
                assert_eq!(&after[..before.len()], before);
                let unresolved_ids: Vec<_> = before
                    .iter()
                    .filter(|a| a.2.is_some())
                    .map(|a| a.0)
                    .collect();
                assert_eq!(
                    after[before.len()..]
                        .iter()
                        .map(|a| a.0)
                        .collect::<Vec<_>>(),
                    unresolved_ids
                );
                assert!(after[before.len()..].iter().all(|a| a.2.is_none()));
                assert_eq!(primary(&failure).serial, original_serial);
                fake.assert_clean();
                assert!(fake.counts.dropped_errors.borrow().is_empty());
                drop(failure);
                assert_dropped_once(&fake.counts);
                scenarios += 1;
            }
        }
        assert_eq!(scenarios, 19 * 64);
    });
}

#[test]
fn successful_commit_never_runs_compensation_even_when_all_cleanup_would_fail() {
    with_writer(|writer| {
        let mut fake = Fake::new(writer, CLEANUP.iter().map(|e| Fault::Before(*e)));
        let result = invoke(writer, &mut fake, &["1", "2", "3"])
            .ok()
            .expect("commit");
        assert_eq!(result.id(), id());
        assert_eq!(result.application(), "fault-test");
        assert!(result.links().is_empty());
        assert_eq!(fake.events, FORWARD);
        assert_eq!(fake.records, BTreeSet::from([7, 42]));
        assert_eq!(fake.map_sets, BTreeSet::from([7, 42]));
        assert_eq!(
            fake.resources,
            baseline()
                .union(&BTreeSet::from([
                    Resource::Program,
                    Resource::Directory,
                    Resource::Map(1),
                    Resource::Map(2),
                    Resource::Map(3),
                    Resource::Bytecode
                ]))
                .copied()
                .collect()
        );
        assert_eq!(fake.counts.handles.get(), 0);
        assert_dropped_once(&fake.counts);
    });
}

#[test]
fn empty_map_set_still_owns_and_cleans_its_directory() {
    with_writer(|writer| {
        let mut fake = Fake::new(writer, [Fault::BeforeCommit]);
        let failure = failed(invoke(writer, &mut fake, &[]));
        assert_eq!(
            &fake.events[fake.events.len() - 3..],
            &[
                Event::RemoveBytecode,
                Event::RemoveProgram,
                Event::RemoveDirectory
            ]
        );
        fake.assert_clean();
        drop(failure);
        assert_dropped_once(&fake.counts);
    });
}

#[test]
fn repeated_total_cleanup_failure_then_recovery_retains_every_attempt() {
    with_writer(|writer| {
        let mut fake = Fake::new(
            writer,
            CLEANUP
                .iter()
                .map(|e| Fault::Before(*e))
                .chain([Fault::BeforeCommit]),
        );
        let mut failure = failed(invoke(writer, &mut fake, &["1", "2", "3"]));
        for pass in 1..=3 {
            assert_eq!(history(&failure).len(), 5 * pass);
            assert_eq!(unresolved(&failure).len(), 6);
            assert!(
                !fake.events.contains(&Event::RemoveDirectory),
                "blocked directory was attempted"
            );
            assert!(fake.counts.dropped_receipts.borrow().is_empty());
            assert!(fake.counts.dropped_errors.borrow().is_empty());
            fake.events.clear();
            failure = retry(writer, &mut fake, failure);
        }
        assert_eq!(history(&failure).len(), 20);
        fake.faults.clear();
        failure = retry(writer, &mut fake, failure);
        assert_eq!(history(&failure).len(), 25);
        fake.assert_clean();
        drop(failure);
        assert_dropped_once(&fake.counts);
    });
}

#[test]
fn wrong_writer_retry_preserves_ownership_until_the_original_runtime_returns() {
    with_writer(|writer| {
        let mut fake = Fake::new(
            writer,
            [Fault::BeforeCommit, Fault::Before(Event::RemoveProgram)],
        );
        let failure = failed(invoke(writer, &mut fake, &["1", "2", "3"]));
        fake.faults.clear();
        with_writer(|other| {
            let failure = retry(other, &mut fake, failure);
            assert_eq!(unresolved(&failure), BTreeSet::from([Resource::Program]));
            assert_eq!(
                history(&failure).last().expect("retry").2,
                Some(Fault::WrongRuntime(Event::RemoveProgram))
            );
            let failure = retry(writer, &mut fake, failure);
            fake.assert_clean();
            drop(failure);
            assert_dropped_once(&fake.counts);
        });
    });
}

#[test]
fn compensation_trace_shows_success_failure_blocking_and_retry() {
    with_writer(|writer| {
        let mut fake = Fake::new(
            writer,
            [
                Fault::BeforeCommit,
                Fault::Before(Event::RemoveProgram),
                Fault::Before(Event::RemoveMap(2)),
            ],
        );
        let failure = failed(invoke(writer, &mut fake, &["1", "2", "3"]));
        assert_eq!(
            unresolved(&failure),
            BTreeSet::from([Resource::Program, Resource::Map(2), Resource::Directory])
        );
        println!("\nOriginal load failure: {}", primary(&failure));
        let removals = fake.events.iter().filter(|e| CLEANUP.contains(e));
        for ((id, _, error), event) in history(&failure).into_iter().zip(removals) {
            println!(
                "  cleanup #{id} {event:?}: {}",
                if error.is_some() {
                    "FAILED (receipt retained)"
                } else {
                    "OK"
                }
            );
        }
        println!("  map directory: BLOCKED by unresolved map pin");
        println!("  residue: {:?}", unresolved(&failure));
        fake.events.clear();
        fake.faults.clear();
        let failure = retry(writer, &mut fake, failure);
        assert_eq!(
            fake.events,
            vec![
                Event::RemoveProgram,
                Event::RemoveMap(2),
                Event::RemoveDirectory
            ]
        );
        for event in &fake.events {
            println!("  retry {event:?}: OK");
        }
        println!(
            "  residue: {:?}; original error and earlier failures retained\n",
            unresolved(&failure)
        );
        fake.assert_clean();
        drop(failure);
        assert_dropped_once(&fake.counts);
    });
}

#[test]
fn directory_retry_waits_for_maps_then_retains_failed_and_successful_attempts() {
    with_writer(|writer| {
        let mut fake = Fake::new(
            writer,
            [
                Fault::BeforeCommit,
                Fault::Before(Event::RemoveMap(2)),
                Fault::Before(Event::RemoveDirectory),
            ],
        );
        let mut failure = failed(invoke(writer, &mut fake, &["1", "2", "3"]));
        let Failure::Compensated {
            directory_attempts, ..
        } = &failure
        else {
            unreachable!("rollback")
        };
        assert!(
            directory_attempts.is_empty(),
            "blocked is not an attempted removal"
        );
        fake.faults.remove(&Fault::Before(Event::RemoveMap(2)));
        fake.events.clear();
        failure = retry(writer, &mut fake, failure);
        assert_eq!(fake.events, [Event::RemoveMap(2), Event::RemoveDirectory]);
        assert_eq!(unresolved(&failure), BTreeSet::from([Resource::Directory]));
        let history_after_maps = history(&failure);
        fake.events.clear();
        failure = retry(writer, &mut fake, failure);
        assert_eq!(fake.events, [Event::RemoveDirectory]);
        assert_eq!(
            history(&failure),
            history_after_maps,
            "successful independent cleanup was retried"
        );
        fake.faults.clear();
        fake.events.clear();
        failure = retry(writer, &mut fake, failure);
        assert_eq!(fake.events, [Event::RemoveDirectory]);
        let Failure::Compensated {
            directory_attempts, ..
        } = &failure
        else {
            unreachable!("rollback")
        };
        assert_eq!(
            directory_attempts
                .iter()
                .map(|r| r.as_ref().err().map(|e| e.fault))
                .collect::<Vec<_>>(),
            [
                Some(Fault::Before(Event::RemoveDirectory)),
                Some(Fault::Before(Event::RemoveDirectory)),
                None
            ]
        );
        fake.assert_clean();
        assert!(fake.counts.dropped_errors.borrow().is_empty());
        drop(failure);
        assert_dropped_once(&fake.counts);
    });
}

mod store;
