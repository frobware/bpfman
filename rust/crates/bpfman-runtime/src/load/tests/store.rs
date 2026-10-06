//! Replace only persistence; keep production routing for open and commit.

use super::*;
use crate::store::testing::Faults;

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

struct StoreEffects<'a, S> {
    fake: Fake,
    store: &'a Faults<S>,
}

impl<S> LoadCleanup for StoreEffects<'_, S> {
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

impl<S> CleanupEffects for StoreEffects<'_, S> {
    type MapDirectory = Directory;

    fn remove_map_directory(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Directory,
    ) -> Result<(), EffectFailure<Directory, Error>> {
        self.fake.remove_map_directory(w, r).map_err(mapped)
    }
}

impl<S: bpfman_store::OpenStore + bpfman_store::CommitLoad> LoadEffects for StoreEffects<'_, S> {
    type Store = <Faults<S> as bpfman_store::OpenStore>::Reader;
    type Prepared = ();
    type Kernel = Kernel;

    fn cancelled(&self) -> Error {
        LoadCause::Cancelled.into()
    }

    fn batch_aborted(&self) -> Error {
        LoadCause::BatchAborted.into()
    }

    fn open_store(&mut self, w: &RuntimeWriter<'_>) -> Result<Self::Store, Error> {
        real::Effects(self.store, &bpfman_kernel_aya::Kernel)
            .open_store(w)
            .map_err(Into::into)
    }

    fn persist(
        &mut self,
        w: &RuntimeWriter<'_>,
        records: &[(NonZeroU32, &Inputs<'_>)],
    ) -> Result<(), Error> {
        real::Effects(self.store, &bpfman_kernel_aya::Kernel)
            .persist(w, records)
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

    fn map_names(kernel: &Kernel) -> &[String] {
        Fake::map_names(kernel)
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

fn independent_store_commit_failure_runs_production_compensation<
    S: bpfman_store::OpenStore + bpfman_store::CommitLoad,
>(
    backend: S,
) {
    with_writer(|writer| {
        let store = Faults::new(backend);
        bpfman_store::OpenStore::open(&store, writer).expect("initialize real store");
        store.clear_calls();
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
        assert!(
            store
                .records(
                    &bpfman_fs::RuntimeDirectory::open_existing(writer.layout().clone())
                        .expect("runtime")
                        .expect("exists")
                )
                .is_empty()
        );
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
        assert!(writer.database_path().exists());
    });
}

fn independent_store_open_failure_precedes_acquisition_and_commit_success_ends_cleanup<
    S: bpfman_store::OpenStore + bpfman_store::CommitLoad,
>(
    backend: S,
) {
    with_writer(|writer| {
        let store = Faults::new(backend);
        bpfman_store::OpenStore::open(&store, writer).expect("initialize real store");
        store.clear_calls();
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
        assert_eq!(
            store
                .records(
                    &bpfman_fs::RuntimeDirectory::open_existing(writer.layout().clone())
                        .expect("runtime")
                        .expect("exists")
                )
                .len(),
            1
        );
        assert_eq!(store.calls(), ["open", "open", "commit"]);
        assert!(!effects.fake.events.iter().any(|event| matches!(
            event,
            Event::RemoveProgram
                | Event::RemoveBytecode
                | Event::RemoveMap(_)
                | Event::RemoveDirectory
        )));
        assert!(writer.database_path().exists());
    });
}

macro_rules! backend_tests {
    ($module:ident, $backend:expr) => {
        mod $module {
            #[test]
            fn independent_store_commit_failure_runs_production_compensation() { super::independent_store_commit_failure_runs_production_compensation($backend); }
            #[test]
            fn independent_store_open_failure_precedes_acquisition_and_commit_success_ends_cleanup() { super::independent_store_open_failure_precedes_acquisition_and_commit_success_ends_cleanup($backend); }
        }
    };
}

backend_tests!(sqlite, bpfman_store_sqlite::Backend);
backend_tests!(json, bpfman_store_json::Backend);
