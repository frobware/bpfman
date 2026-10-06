//! Fault controls around a real persistent store, never an alternate store.
#![allow(clippy::expect_used)]

use bpfman_core::EffectFailure;
use bpfman_fs::{RuntimeDirectory, RuntimeWriter};
use bpfman_model::{StoredProgram, StoredProgramSummary};
use bpfman_store::{
    CommitLoad, Error, ErrorKind, LoadRecord, OpenStore, ProgramReader, UnloadObservation,
    UnloadStore,
};
use std::{
    collections::BTreeMap,
    num::NonZeroU32,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub(crate) struct Faults<S> {
    backend: S,
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    calls: Vec<&'static str>,
    faults: BTreeMap<&'static str, ErrorKind>,
}

fn check(state: &Mutex<State>, call: &'static str) -> Result<(), Error> {
    let mut state = state.lock().expect("fault controls");
    state.calls.push(call);
    match state.faults.get(call) {
        Some(kind) => Err(Error::new(*kind, std::io::Error::other(call))),
        None => Ok(()),
    }
}

impl<S> Faults<S> {
    pub(crate) fn new(backend: S) -> Self {
        Self {
            backend,
            state: Arc::default(),
        }
    }

    pub(crate) fn fail(&self, call: &'static str, kind: ErrorKind) {
        self.state
            .lock()
            .expect("fault controls")
            .faults
            .insert(call, kind);
    }

    pub(crate) fn clear_faults(&self) {
        self.state.lock().expect("fault controls").faults.clear();
    }

    pub(crate) fn clear_calls(&self) {
        self.state.lock().expect("fault controls").calls.clear();
    }

    pub(crate) fn calls(&self) -> Vec<&'static str> {
        self.state.lock().expect("fault controls").calls.clone()
    }
}

impl<S: OpenStore> Faults<S> {
    pub(crate) fn records(&self, runtime: &RuntimeDirectory) -> Vec<StoredProgram> {
        self.backend
            .open_reader(runtime)
            .expect("real reader")
            .expect("existing store")
            .read_records()
            .expect("real records")
    }
}

#[derive(Clone)]
pub(crate) struct Reader<R> {
    reader: R,
    state: Arc<Mutex<State>>,
}

impl<S: OpenStore> OpenStore for Faults<S> {
    type Reader = Reader<S::Reader>;

    fn open_reader(&self, runtime: &RuntimeDirectory) -> Result<Option<Self::Reader>, Error> {
        check(&self.state, "open")?;
        Ok(self.backend.open_reader(runtime)?.map(|reader| Reader {
            reader,
            state: self.state.clone(),
        }))
    }

    fn open(&self, writer: &RuntimeWriter<'_>) -> Result<Self::Reader, Error> {
        check(&self.state, "open")?;
        Ok(Reader {
            reader: self.backend.open(writer)?,
            state: self.state.clone(),
        })
    }
}

impl<R: ProgramReader> ProgramReader for Reader<R> {
    fn validate(&mut self) -> Result<(), Error> {
        check(&self.state, "validate")?;
        self.reader.validate()
    }

    fn read_programs(&mut self) -> Result<Vec<StoredProgramSummary>, Error> {
        check(&self.state, "summaries")?;
        self.reader.read_programs()
    }

    fn read_records(&mut self) -> Result<Vec<StoredProgram>, Error> {
        check(&self.state, "records")?;
        self.reader.read_records()
    }
}

impl<S: CommitLoad> CommitLoad for Faults<S> {
    fn commit_programs(
        &self,
        writer: &RuntimeWriter<'_>,
        records: &[LoadRecord<'_>],
    ) -> Result<(), Error> {
        check(&self.state, "commit")?;
        self.backend.commit_programs(writer, records)
    }
}

impl<S: UnloadStore> UnloadStore for Faults<S> {
    type ProgramReceipt = S::ProgramReceipt;
    type MapSetReceipt = S::MapSetReceipt;

    fn observe_unload(
        &self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<UnloadObservation<Self>, Error> {
        check(&self.state, "observe")?;
        self.backend.observe_unload(writer, id)
    }

    fn delete_program(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::ProgramReceipt,
    ) -> Result<(), EffectFailure<Self::ProgramReceipt, Error>> {
        if let Err(cause) = check(&self.state, "delete program") {
            return Err(EffectFailure {
                cause,
                remaining: receipt,
            });
        }
        self.backend.delete_program(writer, receipt)
    }

    fn delete_map_set(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::MapSetReceipt,
    ) -> Result<(), EffectFailure<Self::MapSetReceipt, Error>> {
        if let Err(cause) = check(&self.state, "delete map set") {
            return Err(EffectFailure {
                cause,
                remaining: receipt,
            });
        }
        self.backend.delete_map_set(writer, receipt)
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
        self.backend.create_pending_tracepoint(writer, request)
    }

    fn finalise_link(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::LinkReceipt,
        kernel_id: NonZeroU32,
    ) -> Result<bpfman_model::StoredLink, EffectFailure<Self::LinkReceipt, Error>> {
        self.backend.finalise_link(writer, receipt, kernel_id)
    }

    fn observe_link(
        &self,
        writer: &RuntimeWriter<'_>,
        id: std::num::NonZeroU64,
    ) -> Result<bpfman_store::LinkObservation<Self>, Error> {
        self.backend.observe_link(writer, id)
    }

    fn delete_link(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::LinkReceipt,
    ) -> Result<(), EffectFailure<Self::LinkReceipt, Error>> {
        self.backend.delete_link(writer, receipt)
    }
}

impl<S: bpfman_store::XdpStore> bpfman_store::XdpStore for Faults<S> {
    type XdpReceipt = S::XdpReceipt;

    fn preflight_xdp(
        &self,
        w: &RuntimeWriter<'_>,
        key: bpfman_model::XdpKey,
        program: NonZeroU32,
    ) -> Result<(), Error> {
        self.backend.preflight_xdp(w, key, program)
    }

    fn commit_xdp(
        &self,
        w: &RuntimeWriter<'_>,
        request: bpfman_store::XdpCommit<'_>,
    ) -> Result<bpfman_model::StoredLink, Error> {
        self.backend.commit_xdp(w, request)
    }

    fn observe_xdp(
        &self,
        w: &RuntimeWriter<'_>,
        id: std::num::NonZeroU64,
    ) -> Result<Option<(bpfman_model::XdpSnapshot, Self::XdpReceipt)>, Error> {
        self.backend.observe_xdp(w, id)
    }

    fn delete_xdp(
        &self,
        w: &RuntimeWriter<'_>,
        receipt: Self::XdpReceipt,
    ) -> Result<(), EffectFailure<Self::XdpReceipt, Error>> {
        self.backend.delete_xdp(w, receipt)
    }
}

impl<S: bpfman_store::XdpReplacementStore> bpfman_store::XdpReplacementStore for Faults<S> {
    fn observe_xdp_dispatcher(
        &self,
        w: &RuntimeWriter<'_>,
        key: bpfman_model::XdpKey,
    ) -> Result<Option<(bpfman_model::XdpDispatcherSnapshot, Self::XdpReceipt)>, Error> {
        self.backend.observe_xdp_dispatcher(w, key)
    }

    fn replace_xdp(
        &self,
        w: &RuntimeWriter<'_>,
        receipt: Self::XdpReceipt,
        request: bpfman_store::XdpReplace<'_>,
    ) -> Result<bpfman_model::XdpDispatcherSnapshot, EffectFailure<Self::XdpReceipt, Error>> {
        self.backend.replace_xdp(w, receipt, request)
    }
}
