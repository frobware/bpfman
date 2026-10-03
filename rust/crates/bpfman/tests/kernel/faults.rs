//! Backend-independent fault decorator. No production switches or storage mutation.

use bpfman_core::EffectFailure;
use bpfman_fs::RuntimeWriter;
use bpfman_model::{StoredProgram, StoredProgramSummary};
use bpfman_store::{
    CommitLoad, Error, ErrorKind, OpenStore, ProgramReader, TracepointRecord, UnloadObservation,
    UnloadStore,
};
use std::{
    num::NonZeroU32,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Point {
    Commit,
    ReadAfterCommit,
    ObserveUnload,
    DeleteProgram,
    DeleteMapSet,
}

#[derive(Default)]
struct State {
    fault: Option<Point>,
    committed: bool,
    calls: Vec<Point>,
}

pub(super) struct Faults<S> {
    pub(super) backend: S,
    state: Arc<Mutex<State>>,
}

impl<S> Faults<S> {
    pub(super) fn new(backend: S) -> Self {
        Self {
            backend,
            state: Arc::default(),
        }
    }

    pub(super) fn set(&self, fault: Option<Point>) {
        self.state.lock().expect("fault state").fault = fault;
    }

    pub(super) fn count(&self, point: Point) -> usize {
        self.state
            .lock()
            .expect("fault state")
            .calls
            .iter()
            .filter(|p| **p == point)
            .count()
    }
}

fn check(state: &Arc<Mutex<State>>, point: Point) -> Result<(), Error> {
    let mut state = state.lock().expect("fault state");
    state.calls.push(point);
    if state.fault == Some(point) {
        Err(Error::new(
            ErrorKind::Unavailable,
            std::io::Error::other(format!("injected {point:?}")),
        ))
    } else {
        Ok(())
    }
}

pub(super) struct Reader<R> {
    reader: R,
    state: Arc<Mutex<State>>,
}

impl<S: OpenStore> OpenStore for Faults<S> {
    type Reader = Reader<S::Reader>;

    fn open(&self, writer: &RuntimeWriter<'_>) -> Result<Self::Reader, Error> {
        Ok(Reader {
            reader: self.backend.open(writer)?,
            state: self.state.clone(),
        })
    }
}

impl<R: ProgramReader> ProgramReader for Reader<R> {
    fn read_programs(&mut self) -> Result<Vec<StoredProgramSummary>, Error> {
        self.reader.read_programs()
    }

    fn read_records(&mut self) -> Result<Vec<StoredProgram>, Error> {
        if self.state.lock().expect("fault state").committed {
            check(&self.state, Point::ReadAfterCommit)?;
        }
        self.reader.read_records()
    }
}

impl<S: CommitLoad> CommitLoad for Faults<S> {
    fn commit_tracepoint(
        &self,
        w: &RuntimeWriter<'_>,
        record: TracepointRecord<'_>,
    ) -> Result<StoredProgramSummary, Error> {
        check(&self.state, Point::Commit)?;
        let result = self.backend.commit_tracepoint(w, record)?;
        self.state.lock().expect("fault state").committed = true;
        Ok(result)
    }
}

impl<S: UnloadStore> UnloadStore for Faults<S> {
    type ProgramReceipt = S::ProgramReceipt;
    type MapSetReceipt = S::MapSetReceipt;

    fn observe_unload(
        &self,
        w: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<UnloadObservation<Self>, Error> {
        check(&self.state, Point::ObserveUnload)?;
        self.backend.observe_unload(w, id)
    }

    fn delete_program(
        &self,
        w: &RuntimeWriter<'_>,
        receipt: Self::ProgramReceipt,
    ) -> Result<(), EffectFailure<Self::ProgramReceipt, Error>> {
        if let Err(cause) = check(&self.state, Point::DeleteProgram) {
            return Err(EffectFailure {
                cause,
                remaining: receipt,
            });
        }
        self.backend.delete_program(w, receipt)
    }

    fn delete_map_set(
        &self,
        w: &RuntimeWriter<'_>,
        receipt: Self::MapSetReceipt,
    ) -> Result<(), EffectFailure<Self::MapSetReceipt, Error>> {
        if let Err(cause) = check(&self.state, Point::DeleteMapSet) {
            return Err(EffectFailure {
                cause,
                remaining: receipt,
            });
        }
        self.backend.delete_map_set(w, receipt)
    }
}
