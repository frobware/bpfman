//! Backend-independent fault decorator. Store failures use domain operations;
//! one filesystem replacement also exercises retained load cleanup.

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
    Validate,
    ReadSummaries,
    ReadRecords,
    Commit,
    CommitWithBlockedCleanup,
    ReadAfterCommit,
    ObserveUnload,
    DeleteProgram,
    DeleteMapSet,
    CreateLink,
    FinaliseLink,
    FinaliseWithBlockedPin,
    ObserveLink,
    DeleteLink,
}

#[derive(Default)]
struct State {
    faults: Vec<Point>,
    successes_before_fault: usize,
    cancellation: Option<(Point, bpfman_runtime::Cancellation)>,
    committed: bool,
    calls: Vec<Point>,
    blocked_bytecode: Option<NonZeroU32>,
    pending_link: Option<std::num::NonZeroU64>,
}

#[derive(Clone)]
pub(super) struct Faults<S> {
    backend: S,
    state: Arc<Mutex<State>>,
}

impl<S> Faults<S> {
    pub(super) fn new(backend: S) -> Self {
        Self {
            backend,
            state: Arc::default(),
        }
    }

    pub(super) fn cancel_at(&self, point: Point, cancellation: &bpfman_runtime::Cancellation) {
        self.state.lock().expect("fault state").cancellation = Some((point, cancellation.clone()));
    }

    pub(super) fn set(&self, fault: Option<Point>) {
        let mut state = self.state.lock().expect("fault state");
        state.faults = fault.into_iter().collect();
        state.successes_before_fault = 0;
    }

    pub(super) fn set_many(&self, faults: &[Point]) {
        let mut state = self.state.lock().expect("fault state");
        state.faults = faults.to_vec();
        state.successes_before_fault = 0;
    }

    pub(super) fn fail_after(&self, point: Point, successes: usize) {
        let mut state = self.state.lock().expect("fault state");
        state.faults = vec![point];
        state.successes_before_fault = successes;
    }

    pub(super) fn blocked_bytecode(&self) -> NonZeroU32 {
        self.state
            .lock()
            .expect("fault state")
            .blocked_bytecode
            .expect("blocked bytecode")
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
    if let Some((boundary, cancellation)) = &state.cancellation {
        if *boundary == point {
            cancellation.cancel();
        }
    }

    if state.faults.contains(&point) && state.successes_before_fault > 0 {
        state.successes_before_fault -= 1;
        return Ok(());
    }

    if state.faults.contains(&point) {
        Err(Error::new(
            ErrorKind::Unavailable,
            std::io::Error::other(format!("injected {point:?}")),
        ))
    } else {
        Ok(())
    }
}

#[derive(Clone)]
pub(super) struct Reader<R> {
    reader: R,
    state: Arc<Mutex<State>>,
}

impl<S: OpenStore> OpenStore for Faults<S> {
    type Reader = Reader<S::Reader>;

    fn open_reader(
        &self,
        runtime: &bpfman_fs::RuntimeDirectory,
    ) -> Result<Option<Self::Reader>, Error> {
        Ok(self.backend.open_reader(runtime)?.map(|reader| Reader {
            reader,
            state: self.state.clone(),
        }))
    }

    fn open(&self, writer: &RuntimeWriter<'_>) -> Result<Self::Reader, Error> {
        Ok(Reader {
            reader: self.backend.open(writer)?,
            state: self.state.clone(),
        })
    }
}

impl<R: ProgramReader> ProgramReader for Reader<R> {
    fn validate(&mut self) -> Result<(), Error> {
        check(&self.state, Point::Validate)?;
        self.reader.validate()
    }

    fn read_programs(&mut self) -> Result<Vec<StoredProgramSummary>, Error> {
        check(&self.state, Point::ReadSummaries)?;
        self.reader.read_programs()
    }

    fn read_records(&mut self) -> Result<Vec<StoredProgram>, Error> {
        check(&self.state, Point::ReadRecords)?;
        if self.state.lock().expect("fault state").committed {
            check(&self.state, Point::ReadAfterCommit)?;
        }

        self.reader.read_records()
    }
}

impl<S: CommitLoad> CommitLoad for Faults<S> {
    fn commit_tracepoints(
        &self,
        w: &RuntimeWriter<'_>,
        records: &[TracepointRecord<'_>],
    ) -> Result<(), Error> {
        if self
            .state
            .lock()
            .expect("fault state")
            .faults
            .contains(&Point::CommitWithBlockedCleanup)
        {
            // Replace only this request's bytecode directory. Cleanup must refuse
            // the symlink and retain the original directory/file receipts.
            let record = records.first().expect("nonempty fault batch");
            let bytecode = w.layout().bytecode_path(record.id);
            let directory = bytecode.parent().expect("program directory");
            let saved = w.layout().root().join("saved-bytecode");
            std::fs::rename(directory, &saved).expect("save bytecode");
            std::os::unix::fs::symlink(&saved, directory).expect("block cleanup");
            self.state.lock().expect("fault state").blocked_bytecode = Some(record.id);
            check(&self.state, Point::CommitWithBlockedCleanup)?;
        }

        check(&self.state, Point::Commit)?;
        self.backend.commit_tracepoints(w, records)?;
        self.state.lock().expect("fault state").committed = true;

        Ok(())
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

impl<R: bpfman_store::LinkReader> bpfman_store::LinkReader for Reader<R> {
    fn read_links(&mut self) -> Result<Vec<bpfman_model::StoredLink>, Error> {
        self.reader.read_links()
    }
}

impl<S: bpfman_store::LinkStore> bpfman_store::LinkStore for Faults<S> {
    type LinkReceipt = S::LinkReceipt;

    fn create_pending_tracepoint(
        &self,
        writer: &RuntimeWriter<'_>,
        request: bpfman_store::PendingTracepoint<'_>,
    ) -> Result<(bpfman_model::StoredLink, Self::LinkReceipt), Error> {
        check(&self.state, Point::CreateLink)?;
        let result = self.backend.create_pending_tracepoint(writer, request)?;
        self.state.lock().expect("state").pending_link = Some(result.0.id);
        Ok(result)
    }

    fn finalise_link(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::LinkReceipt,
        kernel_id: NonZeroU32,
    ) -> Result<bpfman_model::StoredLink, EffectFailure<Self::LinkReceipt, Error>> {
        let result = (|| {
            if self
                .state
                .lock()
                .expect("state")
                .faults
                .contains(&Point::FinaliseWithBlockedPin)
            {
                let id = self
                    .state
                    .lock()
                    .expect("state")
                    .pending_link
                    .expect("pending link");
                std::fs::rename(
                    writer.layout().link_pin_path(id),
                    writer.layout().root().join("fs/held-link"),
                )
                .expect("move this link pin to block cleanup");
                // Leave an explicit obstruction: a fresh observer must reject
                // the path, not mistake a deliberately hidden pin for absence.
                std::fs::create_dir(writer.layout().link_pin_path(id))
                    .expect("obstruct the canonical link pin");
                check(&self.state, Point::FinaliseWithBlockedPin)?;
            }
            check(&self.state, Point::FinaliseLink)
        })();

        if let Err(cause) = result {
            return Err(EffectFailure {
                cause,
                remaining: receipt,
            });
        }
        self.backend.finalise_link(writer, receipt, kernel_id)
    }

    fn observe_link(
        &self,
        writer: &RuntimeWriter<'_>,
        id: std::num::NonZeroU64,
    ) -> Result<bpfman_store::LinkObservation<Self>, Error> {
        check(&self.state, Point::ObserveLink)?;
        self.backend.observe_link(writer, id)
    }

    fn delete_link(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::LinkReceipt,
    ) -> Result<(), EffectFailure<Self::LinkReceipt, Error>> {
        if let Err(cause) = check(&self.state, Point::DeleteLink) {
            return Err(EffectFailure {
                cause,
                remaining: receipt,
            });
        }
        self.backend.delete_link(writer, receipt)
    }
}

pub(super) fn restore_blocked_pin(writer: &RuntimeWriter<'_>, id: std::num::NonZeroU64) {
    std::fs::rename(
        writer.layout().link_pin_path(id),
        writer.layout().root().join("fs/removed-obstruction"),
    )
    .expect("move obstruction away from the owned pin");
    std::fs::rename(
        writer.layout().root().join("fs/held-link"),
        writer.layout().link_pin_path(id),
    )
    .expect("restore owned pin");
}
