//! Replace only persistence; keep production routing for open and commit.

use super::*;
use crate::store::testing::Memory;

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Store(#[from] LoadCause),
    #[error(transparent)]
    Fake(#[from] TestError),
}

fn mapped<R>(failure: EffectFailure<R, TestError>) -> EffectFailure<R, Error> {
    EffectFailure {
        remaining: failure.remaining,
        cause: failure.cause.into(),
    }
}

struct StoreEffects<'a> {
    fake: Fake,
    store: &'a Memory,
}

impl LoadCleanup for StoreEffects<'_> {
    type ProgramPin = Program;
    type MapPin = Map;
    type Bytecode = BytecodeReceipt;
    type Error = Error;

    fn remove_bytecode(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: BytecodeReceipt,
    ) -> Result<(), EffectFailure<BytecodeReceipt, Error>> {
        self.fake.remove_bytecode(w, r).map_err(mapped)
    }

    fn remove_program_pin(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Program,
    ) -> Result<(), EffectFailure<Program, Error>> {
        self.fake.remove_program_pin(w, r).map_err(mapped)
    }

    fn remove_map_pin(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Map,
    ) -> Result<(), EffectFailure<Map, Error>> {
        self.fake.remove_map_pin(w, r).map_err(mapped)
    }
}

impl CleanupEffects for StoreEffects<'_> {
    type MapDirectory = Directory;

    fn remove_map_directory(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Directory,
    ) -> Result<(), EffectFailure<Directory, Error>> {
        self.fake.remove_map_directory(w, r).map_err(mapped)
    }
}

impl LoadEffects for StoreEffects<'_> {
    type Store = Memory;
    type Prepared = ();
    type Kernel = Kernel;

    fn open_store(&mut self, w: &RuntimeWriter<'_>) -> Result<Memory, Error> {
        real::Effects(self.store).open_store(w).map_err(Into::into)
    }

    fn persist(
        &mut self,
        w: &RuntimeWriter<'_>,
        id: NonZeroU32,
        input: &Inputs<'_>,
    ) -> Result<StoredProgramSummary, Error> {
        real::Effects(self.store)
            .persist(w, id, input)
            .map_err(Into::into)
    }

    fn prepare(&mut self, w: &RuntimeWriter<'_>) -> Result<(), Error> {
        self.fake.prepare(w).map_err(Into::into)
    }

    fn load_kernel(&mut self, w: &RuntimeWriter<'_>, i: &Inputs<'_>) -> Result<Kernel, Error> {
        self.fake.load_kernel(w, i).map_err(Into::into)
    }

    fn pin_program(
        &mut self,
        w: &RuntimeWriter<'_>,
        p: &(),
        k: &mut Kernel,
        n: &Symbol,
    ) -> Result<Program, EffectFailure<Option<Program>, Error>> {
        self.fake.pin_program(w, p, k, n).map_err(mapped)
    }

    fn program_id(p: &Program) -> NonZeroU32 {
        Fake::program_id(p)
    }

    fn create_map_directory(
        &mut self,
        w: &RuntimeWriter<'_>,
        p: &(),
        id: NonZeroU32,
    ) -> Result<Directory, EffectFailure<Option<Directory>, Error>> {
        self.fake.create_map_directory(w, p, id).map_err(mapped)
    }

    fn pin_map(
        &mut self,
        w: &RuntimeWriter<'_>,
        k: &Kernel,
        d: &Directory,
        n: &str,
    ) -> Result<Map, EffectFailure<Option<Map>, Error>> {
        self.fake.pin_map(w, k, d, n).map_err(mapped)
    }

    fn publish(
        &mut self,
        w: &RuntimeWriter<'_>,
        id: NonZeroU32,
        i: &Inputs<'_>,
    ) -> Result<BytecodeReceipt, EffectFailure<Vec<BytecodeReceipt>, Error>> {
        self.fake.publish(w, id, i).map_err(mapped)
    }
}

#[test]
fn independent_store_commit_failure_runs_production_compensation() {
    with_writer(|writer| {
        let store = Memory::new(writer);
        store.fail("commit", bpfman_store::ErrorKind::Unavailable);
        let mut effects = StoreEffects {
            fake: Fake::new(
                writer,
                [
                    Fault::Before(Event::RemoveProgram),
                    Fault::Before(Event::RemoveMap(2)),
                ],
            ),
            store: &store,
        };
        let failure = match invoke(writer, &mut effects, &["1", "2"]) {
            Err(f) => f,
            Ok(_) => unreachable!("injected commit must fail"),
        };
        assert_eq!(store.calls(), ["open", "commit"]);
        assert_eq!(store.residue(), (false, false));
        assert_eq!(
            effects.fake.resources,
            BTreeSet::from([
                Resource::Unrelated,
                Resource::SharedMap,
                Resource::Program,
                Resource::Map(2),
                Resource::Directory
            ])
        );
        let Failure::Compensated { report, .. } = &failure else {
            unreachable!("must compensate")
        };
        assert!(
            matches!(report.primary(),Error::Store(LoadCause::Store(e)) if e.kind()==bpfman_store::ErrorKind::Unavailable)
        );
        assert_eq!(report.attempts().len(), 4);
        effects.fake.faults.clear();
        let failure = retry(writer, &mut effects, failure);
        effects.fake.assert_clean();
        let Failure::Compensated {
            report, directory, ..
        } = &failure
        else {
            unreachable!("history")
        };
        assert!(report.remaining().is_empty());
        assert!(directory.is_none());
        assert_eq!(report.attempts().len(), 6);

        // Explicit cleanup never repeats the failed store commit.
        assert_eq!(store.calls(), ["open", "commit"]);
        assert!(!writer.database_path().exists());
    });
}

#[test]
fn independent_store_open_failure_precedes_acquisition_and_commit_success_ends_cleanup() {
    with_writer(|writer| {
        let store = Memory::new(writer);
        store.fail("open", bpfman_store::ErrorKind::IncompatibleState);
        let mut effects = StoreEffects {
            fake: Fake::new(writer, []),
            store: &store,
        };
        assert!(matches!(
            invoke(writer, &mut effects, &["1"]),
            Err(Failure::NoOwnedArtifacts(_))
        ));
        assert!(effects.fake.events.is_empty());
        effects.fake.assert_clean();
        store.clear_faults();
        effects.fake.faults.extend([
            Fault::Before(Event::RemoveProgram),
            Fault::Before(Event::RemoveBytecode),
        ]);
        assert!(invoke(writer, &mut effects, &["1"]).is_ok());
        assert_eq!(store.residue(), (true, true));
        assert_eq!(store.calls(), ["open", "open", "commit"]);
        assert!(!effects.fake.events.iter().any(|event| matches!(
            event,
            Event::RemoveProgram
                | Event::RemoveBytecode
                | Event::RemoveMap(_)
                | Event::RemoveDirectory
        )));
        assert!(!writer.database_path().exists());
    });
}
