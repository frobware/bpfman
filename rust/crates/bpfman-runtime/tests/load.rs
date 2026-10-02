//! Stateful fake exercises the production compensation interpreter under a real
//! writer scope. No kernel/bpffs operations occur; real confinement is tested
//! separately in bpfman-fs.
#![allow(clippy::expect_used)]

use std::{
    cell::{Cell, RefCell},
    collections::BTreeSet,
    path::PathBuf,
    rc::Rc,
    time::Duration,
};

use bpfman_core::{
    EffectFailure, KernelAcquisitions, LoadCompensation, LoadComplete, LoadProgram, LoadRollback,
};
use bpfman_fs::{RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use bpfman_lock::AcquireOptions;
use bpfman_model::{ProgramSpec, ProgramType, Symbol};
use bpfman_runtime::{LoadCleanup, compensate_load};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Resource {
    Program(u32),
    Map(u32),
    Staging(u32),
    Bytecode(u32),
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Event {
    Load,
    PinProgram,
    PinMap(u32),
    Publish,
    Persist,
    Remove(Resource),
}

// Neither errors nor receipts are Clone. Counters detect accidental loss of
// error history and opaque ownership in the pure core and interpreter.
#[derive(Debug, thiserror::Error)]
#[error("{reason} at {event:?}")]
struct FakeError {
    event: Event,
    reason: &'static str,
    drops: Rc<Cell<usize>>,
}
impl Drop for FakeError {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}

struct Receipt {
    resource: Resource,
    authority: Rc<()>,
    drops: Rc<RefCell<Vec<Resource>>>,
}
impl Drop for Receipt {
    fn drop(&mut self) {
        self.drops.borrow_mut().push(self.resource);
    }
}
struct ProgramPin(Receipt);
struct MapPin(Receipt);
struct Bytecode(Receipt);

type Rollback = LoadRollback<ProgramPin, MapPin, Bytecode, FakeError>;
type Complete = LoadComplete<ProgramPin, MapPin, Bytecode, u32>;
type KernelFailure = EffectFailure<KernelAcquisitions<ProgramPin, MapPin>, FakeError>;

struct Fake {
    runtime: PathBuf,
    authority: Rc<()>,
    resources: BTreeSet<Resource>,
    records: BTreeSet<u32>,
    faults: BTreeSet<Event>,
    events: Vec<Event>,
    receipt_drops: Rc<RefCell<Vec<Resource>>>,
    error_drops: Rc<Cell<usize>>,
}

fn preexisting() -> BTreeSet<Resource> {
    // Map 8 represents borrowed/shared state, never an owned cleanup receipt.
    BTreeSet::from([
        Resource::Program(7),
        Resource::Map(7),
        Resource::Map(8),
        Resource::Bytecode(7),
    ])
}

impl Fake {
    fn new(writer: &RuntimeWriter<'_>, faults: &[Event]) -> Self {
        Self {
            runtime: writer.database_path(),
            authority: Rc::new(()),
            resources: preexisting(),
            records: BTreeSet::from([7]),
            faults: faults.iter().copied().collect(),
            events: Vec::new(),
            receipt_drops: Rc::default(),
            error_drops: Rc::default(),
        }
    }

    fn error(&self, event: Event, reason: &'static str) -> FakeError {
        FakeError {
            event,
            reason,
            drops: self.error_drops.clone(),
        }
    }

    fn check(&mut self, event: Event) -> Result<(), FakeError> {
        self.events.push(event);
        if self.faults.contains(&event) {
            Err(self.error(event, "injected failure"))
        } else {
            Ok(())
        }
    }

    fn acquire(&mut self, resource: Resource) -> Receipt {
        assert!(self.resources.insert(resource), "resource already exists");
        Receipt {
            resource,
            authority: self.authority.clone(),
            drops: self.receipt_drops.clone(),
        }
    }

    fn load(&mut self, spec: &ProgramSpec) -> Result<(ProgramPin, Vec<MapPin>), KernelFailure> {
        assert_eq!(spec.kind(), ProgramType::Tracepoint);
        assert_eq!(spec.name().as_str(), "tracepoint_kill_recorder");
        let mut acquired = KernelAcquisitions {
            program_pin: None,
            map_pins: Vec::new(),
        };
        for event in [Event::Load, Event::PinProgram] {
            if let Err(cause) = self.check(event) {
                return Err(EffectFailure {
                    remaining: acquired,
                    cause,
                });
            }
        }
        let program = ProgramPin(self.acquire(Resource::Program(42)));
        for id in [101, 102, 103] {
            if let Err(cause) = self.check(Event::PinMap(id)) {
                acquired.program_pin = Some(program);
                return Err(EffectFailure {
                    remaining: acquired,
                    cause,
                });
            }
            acquired
                .map_pins
                .push(MapPin(self.acquire(Resource::Map(id))));
        }
        Ok((program, acquired.map_pins))
    }

    fn publish(
        &mut self,
        program: &ProgramPin,
    ) -> Result<Bytecode, EffectFailure<Vec<Bytecode>, FakeError>> {
        assert_eq!(program.0.resource, Resource::Program(42));
        assert!(self.resources.contains(&program.0.resource));
        let mut staged = Bytecode(self.acquire(Resource::Staging(42)));
        if let Err(cause) = self.check(Event::Publish) {
            // Forward failure returns staging ownership rather than hiding a
            // potentially failing local cleanup in a defer/Drop implementation.
            return Err(EffectFailure {
                remaining: vec![staged],
                cause,
            });
        }
        assert!(self.resources.remove(&Resource::Staging(42)));
        assert!(self.resources.insert(Resource::Bytecode(42)));
        staged.0.resource = Resource::Bytecode(42);
        Ok(staged)
    }

    fn persist(
        &mut self,
        (spec, program, maps, bytecode): (&ProgramSpec, &ProgramPin, &[MapPin], &Bytecode),
    ) -> Result<u32, FakeError> {
        assert_eq!(spec.kind(), ProgramType::Tracepoint);
        assert!(self.resources.contains(&program.0.resource));
        assert_eq!(maps.len(), 3);
        for map in maps {
            assert!(self.resources.contains(&map.0.resource));
        }
        assert_eq!(bytecode.0.resource, Resource::Bytecode(42));
        assert!(self.resources.contains(&bytecode.0.resource));
        self.check(Event::Persist)?;
        assert!(self.records.insert(42));
        Ok(42)
    }

    fn remove(&mut self, writer: &RuntimeWriter<'_>, receipt: &Receipt) -> Result<(), FakeError> {
        assert!(
            !self.records.contains(&42),
            "must not roll back a committed load"
        );
        let event = Event::Remove(receipt.resource);
        self.check(event)?;
        if writer.database_path() != self.runtime
            || !Rc::ptr_eq(&receipt.authority, &self.authority)
        {
            return Err(self.error(event, "wrong runtime authority"));
        }
        assert!(
            self.resources.remove(&receipt.resource),
            "double removal or wrong ownership"
        );
        Ok(())
    }

    fn assert_clean(&self) {
        assert_eq!(self.resources, preexisting());
        assert_eq!(self.records, BTreeSet::from([7]));
    }
}

impl LoadCleanup for Fake {
    type ProgramPin = ProgramPin;
    type MapPin = MapPin;
    type Bytecode = Bytecode;
    type Error = FakeError;

    fn remove_bytecode(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Bytecode,
    ) -> Result<(), EffectFailure<Bytecode, FakeError>> {
        match self.remove(writer, &receipt.0) {
            Ok(()) => {
                drop(receipt);
                Ok(())
            }
            Err(cause) => Err(EffectFailure {
                remaining: receipt,
                cause,
            }),
        }
    }

    fn remove_program_pin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: ProgramPin,
    ) -> Result<(), EffectFailure<ProgramPin, FakeError>> {
        match self.remove(writer, &receipt.0) {
            Ok(()) => {
                drop(receipt);
                Ok(())
            }
            Err(cause) => Err(EffectFailure {
                remaining: receipt,
                cause,
            }),
        }
    }

    fn remove_map_pin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: MapPin,
    ) -> Result<(), EffectFailure<MapPin, FakeError>> {
        match self.remove(writer, &receipt.0) {
            Ok(()) => {
                drop(receipt);
                Ok(())
            }
            Err(cause) => Err(EffectFailure {
                remaining: receipt,
                cause,
            }),
        }
    }
}

fn with_writer(test: impl FnOnce(&RuntimeWriter<'_>)) {
    let temp = tempfile::tempdir().expect("temporary runtime");
    let layout = RuntimeLayout::try_from(temp.path().join("runtime")).expect("layout");
    let directory = RuntimeDirectory::open_or_create(layout).expect("open runtime");
    directory
        .with_writer(
            AcquireOptions {
                timeout: Duration::from_secs(1),
                cancelled: None,
            },
            |writer| test(&writer),
        )
        .expect("writer scope");
}

enum Forward {
    Complete(Complete),
    Rollback(Rollback),
}

fn interpret_forward(fake: &mut Fake) -> Forward {
    let load = LoadProgram::new(ProgramSpec::Tracepoint(
        Symbol::try_from("tracepoint_kill_recorder").expect("fixture"),
    ));
    let (program, maps) = match fake.load(load.spec()) {
        Ok(loaded) => loaded,
        Err(failure) => return Forward::Rollback(load.failed(failure)),
    };
    let publish = load.loaded(program, maps);
    let bytecode = match fake.publish(publish.program_pin()) {
        Ok(bytecode) => bytecode,
        Err(failure) => return Forward::Rollback(publish.failed(failure)),
    };
    let persist = publish.published(bytecode);
    match fake.persist(persist.inputs()) {
        Ok(stored) => Forward::Complete(persist.committed(stored)),
        Err(error) => Forward::Rollback(persist.failed(error)),
    }
}

fn failed_load(fake: &mut Fake) -> Rollback {
    match interpret_forward(fake) {
        Forward::Rollback(rollback) => rollback,
        Forward::Complete(_) => unreachable!("expected a failed load"),
    }
}

fn target(instruction: &LoadCompensation<ProgramPin, MapPin, Bytecode>) -> Resource {
    match instruction {
        LoadCompensation::RemoveBytecode(b) => b.0.resource,
        LoadCompensation::RemoveProgramPin(p) => p.0.resource,
        LoadCompensation::RemoveMapPin(m) => m.0.resource,
    }
}

fn full_cleanup() -> Vec<Resource> {
    vec![
        Resource::Bytecode(42),
        Resource::Program(42),
        Resource::Map(103),
        Resource::Map(102),
        Resource::Map(101),
    ]
}

#[test]
fn committed_load_transfers_receipts_without_scheduling_compensation() {
    with_writer(|writer| {
        let mut fake = Fake::new(writer, &[]);
        let loaded = match interpret_forward(&mut fake) {
            Forward::Complete(loaded) => loaded,
            Forward::Rollback(_) => unreachable!("success expected"),
        };
        assert_eq!(loaded.spec.kind(), ProgramType::Tracepoint);
        assert_eq!(loaded.program_pin.0.resource, Resource::Program(42));
        assert_eq!(loaded.map_pins.len(), 3);
        assert_eq!(loaded.bytecode.0.resource, Resource::Bytecode(42));
        assert_eq!(loaded.stored, 42);
        assert!(fake.receipt_drops.borrow().is_empty());
        assert_eq!(
            fake.events,
            [
                Event::Load,
                Event::PinProgram,
                Event::PinMap(101),
                Event::PinMap(102),
                Event::PinMap(103),
                Event::Publish,
                Event::Persist
            ]
        );
        drop(loaded);
        assert_eq!(fake.receipt_drops.borrow().len(), 5);
        assert_eq!(fake.records, BTreeSet::from([7, 42]));
        assert_eq!(
            fake.resources,
            preexisting()
                .union(&full_cleanup().into_iter().collect())
                .copied()
                .collect()
        );
    });
}

#[test]
fn every_forward_failure_returns_exact_partial_acquisitions_for_cleanup() {
    for (failure, expected) in [
        (Event::Load, vec![]),
        (Event::PinProgram, vec![]),
        (Event::PinMap(101), vec![Resource::Program(42)]),
        (
            Event::PinMap(102),
            vec![Resource::Program(42), Resource::Map(101)],
        ),
        (
            Event::PinMap(103),
            vec![
                Resource::Program(42),
                Resource::Map(102),
                Resource::Map(101),
            ],
        ),
        (
            Event::Publish,
            vec![
                Resource::Staging(42),
                Resource::Program(42),
                Resource::Map(103),
                Resource::Map(102),
                Resource::Map(101),
            ],
        ),
        (Event::Persist, full_cleanup()),
    ] {
        with_writer(|writer| {
            let mut fake = Fake::new(writer, &[failure]);
            let plan = failed_load(&mut fake);
            assert_eq!(
                plan.pending()
                    .map(|p| target(p.instruction()))
                    .collect::<Vec<_>>(),
                expected
            );
            assert!(fake.receipt_drops.borrow().is_empty());
            let before = fake.events.len();
            let report = compensate_load(writer, &mut fake, plan);
            assert_eq!(
                fake.events[before..],
                expected
                    .iter()
                    .copied()
                    .map(Event::Remove)
                    .collect::<Vec<_>>()
            );
            assert_eq!(report.primary().event, failure);
            assert_eq!(report.spec().kind(), ProgramType::Tracepoint);
            assert!(report.remaining().is_empty());
            assert!(report.attempts().iter().all(|a| a.outcome.is_ok()));
            assert_eq!(fake.receipt_drops.borrow().len(), expected.len());
            assert_eq!(fake.error_drops.get(), 0);
            fake.assert_clean();
            drop(report);
            assert_eq!(fake.error_drops.get(), 1);
        });
    }
}

#[test]
fn every_combination_of_cleanup_failures_runs_the_entire_pass_and_retries_only_residue() {
    let actions = full_cleanup();
    // Five instructions: every subset, including all succeeding/all failing.
    for mask in 0u32..(1 << actions.len()) {
        with_writer(|writer| {
            let failures: Vec<_> = actions
                .iter()
                .copied()
                .enumerate()
                .filter(|(i, _)| mask & (1 << i) != 0)
                .collect();
            let faults: Vec<_> = std::iter::once(Event::Persist)
                .chain(failures.iter().map(|(_, r)| Event::Remove(*r)))
                .collect();
            let mut fake = Fake::new(writer, &faults);
            let plan = failed_load(&mut fake);
            let before = fake.events.len();
            let report = compensate_load(writer, &mut fake, plan);
            assert_eq!(
                fake.events[before..],
                actions
                    .iter()
                    .copied()
                    .map(Event::Remove)
                    .collect::<Vec<_>>()
            );
            assert_eq!(report.attempts().len(), actions.len());
            assert_eq!(
                report
                    .remaining()
                    .iter()
                    .map(|p| (p.id(), target(p.instruction())))
                    .collect::<Vec<_>>(),
                failures
            );
            for (i, attempt) in report.attempts().iter().enumerate() {
                assert_eq!(attempt.id, i);
                assert_eq!(attempt.outcome.is_err(), mask & (1 << i) != 0);
                if let Err(error) = &attempt.outcome {
                    assert_eq!(error.event, Event::Remove(actions[i]));
                }
            }
            assert_eq!(
                fake.receipt_drops.borrow().len(),
                actions.len() - failures.len()
            );
            assert_eq!(fake.error_drops.get(), 0);
            assert_eq!(
                fake.resources,
                preexisting()
                    .union(&failures.iter().map(|(_, r)| *r).collect())
                    .copied()
                    .collect()
            );

            // Recovery is explicitly a second pass: the first pass did not
            // retry inline or skip a later pin. Prior errors remain in history.
            fake.faults.clear();
            let plan = report.retry();
            assert_eq!(
                plan.pending()
                    .map(|p| (p.id(), target(p.instruction())))
                    .collect::<Vec<_>>(),
                failures
            );
            let before = fake.events.len();
            let report = compensate_load(writer, &mut fake, plan);
            assert_eq!(
                fake.events[before..],
                failures
                    .iter()
                    .map(|(_, r)| Event::Remove(*r))
                    .collect::<Vec<_>>()
            );
            assert_eq!(report.attempts().len(), actions.len() + failures.len());
            assert_eq!(
                report
                    .attempts()
                    .iter()
                    .filter(|a| a.outcome.is_err())
                    .count(),
                failures.len()
            );
            assert!(report.remaining().is_empty());
            assert_eq!(report.primary().event, Event::Persist);
            assert_eq!(fake.receipt_drops.borrow().len(), actions.len());
            assert_eq!(fake.error_drops.get(), 0);
            fake.assert_clean();
            drop(report);
            assert_eq!(fake.error_drops.get(), 1 + failures.len());
        });
    }
}

#[test]
fn repeated_total_failure_is_bounded_and_keeps_error_history_and_receipts() {
    with_writer(|writer| {
        let actions = full_cleanup();
        let faults: Vec<_> = std::iter::once(Event::Persist)
            .chain(actions.iter().copied().map(Event::Remove))
            .collect();
        let mut fake = Fake::new(writer, &faults);
        let mut plan = failed_load(&mut fake);
        for pass in 1..=3 {
            let before = fake.events.len();
            let report = compensate_load(writer, &mut fake, plan);
            assert_eq!(fake.events.len() - before, actions.len());
            assert_eq!(report.attempts().len(), pass * actions.len());
            assert_eq!(report.remaining().len(), actions.len());
            assert!(fake.receipt_drops.borrow().is_empty());
            assert_eq!(fake.error_drops.get(), 0);
            plan = report.retry();
        }
        fake.faults.clear();
        let report = compensate_load(writer, &mut fake, plan);
        fake.assert_clean();
        assert_eq!(report.attempts().len(), 4 * actions.len());
        assert_eq!(report.primary().event, Event::Persist);
        assert_eq!(fake.error_drops.get(), 0);
        drop(report);
        assert_eq!(fake.error_drops.get(), 1 + 3 * actions.len());
    });
}

#[test]
fn partial_publication_and_partial_kernel_load_also_survive_cleanup_failure() {
    for failure in [Event::Publish, Event::PinMap(103)] {
        with_writer(|writer| {
            let mut fake = Fake::new(writer, &[failure]);
            let plan = failed_load(&mut fake);
            let owned: Vec<_> = plan.pending().map(|p| target(p.instruction())).collect();
            fake.faults.extend(owned.iter().copied().map(Event::Remove));
            let report = compensate_load(writer, &mut fake, plan);
            assert_eq!(report.remaining().len(), owned.len());
            assert_eq!(report.attempts().len(), owned.len());
            assert!(fake.receipt_drops.borrow().is_empty());
            fake.faults.clear();
            let report = compensate_load(writer, &mut fake, report.retry());
            assert!(report.remaining().is_empty());
            assert_eq!(report.primary().event, failure);
            fake.assert_clean();
        });
    }
}

#[test]
fn wrong_runtime_failure_preserves_all_receipts_for_the_right_runtime() {
    with_writer(|writer| {
        let mut fake = Fake::new(writer, &[Event::Persist]);
        let plan = failed_load(&mut fake);
        let resources = fake.resources.clone();
        with_writer(|other_writer| {
            let report = compensate_load(other_writer, &mut fake, plan);
            assert_eq!(report.remaining().len(), 5);
            assert_eq!(report.attempts().len(), 5);
            assert!(
                report
                    .attempts()
                    .iter()
                    .all(|a| matches!(&a.outcome, Err(e) if e.reason == "wrong runtime authority"))
            );
            assert_eq!(fake.resources, resources);
            assert!(fake.receipt_drops.borrow().is_empty());
            let report = compensate_load(writer, &mut fake, report.retry());
            assert!(report.remaining().is_empty());
            fake.assert_clean();
        });
    });
}

#[test]
fn partial_kernel_residue_can_contain_only_maps() {
    with_writer(|writer| {
        let mut fake = Fake::new(writer, &[Event::Remove(Resource::Map(102))]);
        let maps = vec![
            MapPin(fake.acquire(Resource::Map(101))),
            MapPin(fake.acquire(Resource::Map(102))),
        ];
        let load = LoadProgram::new(ProgramSpec::Tracepoint(
            Symbol::try_from("trace").expect("fixture"),
        ));
        let plan = load.failed(EffectFailure {
            remaining: KernelAcquisitions::<ProgramPin, _> {
                program_pin: None,
                map_pins: maps,
            },
            cause: fake.error(Event::PinProgram, "injected failure"),
        });
        let report = compensate_load(writer, &mut fake, plan);
        assert_eq!(
            fake.events,
            [
                Event::Remove(Resource::Map(102)),
                Event::Remove(Resource::Map(101))
            ]
        );
        assert_eq!(report.remaining().len(), 1);
        assert_eq!(
            target(report.remaining()[0].instruction()),
            Resource::Map(102)
        );
        fake.faults.clear();
        let report = compensate_load(writer, &mut fake, report.retry());
        assert!(report.remaining().is_empty());
        fake.assert_clean();
    });
}

#[test]
fn publication_failure_before_staging_schedules_no_bytecode_removal() {
    with_writer(|writer| {
        let mut fake = Fake::new(writer, &[]);
        let load = LoadProgram::new(ProgramSpec::Tracepoint(
            Symbol::try_from("tracepoint_kill_recorder").expect("fixture"),
        ));
        let (program, maps) = match fake.load(load.spec()) {
            Ok(resources) => resources,
            Err(_) => unreachable!("load expected"),
        };
        let plan = load.loaded(program, maps).failed(EffectFailure {
            remaining: Vec::<Bytecode>::new(),
            cause: fake.error(Event::Publish, "injected failure"),
        });
        assert_eq!(plan.pending().len(), 4);
        let before = fake.events.len();
        let report = compensate_load(writer, &mut fake, plan);
        assert_eq!(
            fake.events[before..],
            full_cleanup()[1..]
                .iter()
                .copied()
                .map(Event::Remove)
                .collect::<Vec<_>>()
        );
        assert!(report.remaining().is_empty());
        fake.assert_clean();
    });
}
