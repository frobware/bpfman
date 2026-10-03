//! An independent, in-memory persistence implementation used only in tests.
#![allow(clippy::expect_used)]

use crate::sample;
use bpfman_core::EffectFailure;
use bpfman_fs::{RuntimeIdentity, RuntimeWriter};
use bpfman_model::{ProgramSource, ProgramSpec, StoredProgram, StoredProgramSummary};
use bpfman_store::{
    CommitLoad, Error, ErrorKind, OpenStore, ProgramReader, TracepointRecord, UnloadObservation,
    UnloadStore,
};
use std::{
    collections::BTreeMap,
    num::NonZeroU32,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub(crate) struct Memory {
    root: RuntimeIdentity,
    state: Arc<Mutex<State>>,
}

struct State {
    program: Option<StoredProgram>,
    map_set: bool,
    generation: u64,
    calls: Vec<&'static str>,
    faults: BTreeMap<&'static str, ErrorKind>,
}

pub(crate) struct ProgramReceipt(Receipt);

pub(crate) struct MapSetReceipt(Receipt);

struct Receipt {
    root: RuntimeIdentity,
    owner: Arc<Mutex<State>>,
    generation: u64,
}

fn error(kind: ErrorKind, message: &'static str) -> Error {
    Error::new(kind, std::io::Error::other(message))
}

fn summary(p: &StoredProgram) -> StoredProgramSummary {
    StoredProgramSummary::new(
        p.id,
        p.spec.name().as_str().into(),
        p.spec.kind(),
        p.metadata.clone(),
        p.links.clone(),
    )
}

impl State {
    fn enter(&mut self, call: &'static str) -> Result<(), Error> {
        self.calls.push(call);
        match self.faults.get(call) {
            Some(kind) => Err(error(*kind, call)),
            None => Ok(()),
        }
    }
}

impl Memory {
    pub(crate) fn new(writer: &RuntimeWriter<'_>) -> Self {
        Self {
            root: writer.identity().expect("runtime identity"),
            state: Arc::new(Mutex::new(State {
                program: None,
                map_set: false,
                generation: 0,
                calls: vec![],
                faults: BTreeMap::new(),
            })),
        }
    }

    pub(crate) fn fail(&self, call: &'static str, kind: ErrorKind) {
        self.state.lock().expect("state").faults.insert(call, kind);
    }

    pub(crate) fn clear_faults(&self) {
        self.state.lock().expect("state").faults.clear();
    }

    pub(crate) fn calls(&self) -> Vec<&'static str> {
        self.state.lock().expect("state").calls.clone()
    }

    pub(crate) fn residue(&self) -> (bool, bool) {
        let s = self.state.lock().expect("state");
        (s.program.is_some(), s.map_set)
    }

    fn authority(&self, writer: &RuntimeWriter<'_>) -> Result<(), Error> {
        if writer.identity().expect("runtime identity") != self.root {
            return Err(error(ErrorKind::InvalidData, "wrong runtime"));
        }
        Ok(())
    }

    fn receipt(&self, generation: u64) -> Receipt {
        Receipt {
            root: self.root,
            owner: self.state.clone(),
            generation,
        }
    }

    fn validate(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: &Receipt,
        state: &State,
    ) -> Result<(), Error> {
        self.authority(writer)?;
        if receipt.root != self.root
            || !Arc::ptr_eq(&receipt.owner, &self.state)
            || receipt.generation != state.generation
        {
            return Err(error(ErrorKind::InvalidData, "foreign or stale receipt"));
        }
        Ok(())
    }
}

impl OpenStore for Memory {
    type Reader = Self;

    fn open(&self, writer: &RuntimeWriter<'_>) -> Result<Self, Error> {
        self.authority(writer)?;
        self.state.lock().expect("state").enter("open")?;
        Ok(self.clone())
    }
}

impl ProgramReader for Memory {
    fn read_programs(&mut self) -> Result<Vec<StoredProgramSummary>, Error> {
        let mut s = self.state.lock().expect("state");
        s.enter("summaries")?;
        Ok(s.program.iter().map(summary).collect())
    }

    fn read_records(&mut self) -> Result<Vec<StoredProgram>, Error> {
        let mut s = self.state.lock().expect("state");
        s.enter("records")?;
        Ok(s.program.iter().cloned().collect())
    }
}

impl CommitLoad for Memory {
    fn commit_tracepoint(
        &self,
        writer: &RuntimeWriter<'_>,
        record: TracepointRecord<'_>,
    ) -> Result<StoredProgramSummary, Error> {
        self.authority(writer)?;
        let mut s = self.state.lock().expect("state");
        s.enter("commit")?;
        if s.program.is_some() || s.map_set {
            return Err(error(ErrorKind::InvalidData, "already committed"));
        }
        let mut p = sample::record();
        p.id = record.id;
        p.map_set = record.id;
        p.spec = ProgramSpec::Tracepoint(record.name.clone());
        p.source = ProgramSource::File(Some(record.source.into()));
        p.license = record.license.into();
        p.created_at = record.created_at.into();
        p.metadata = record.metadata.clone();
        p.object_path = writer
            .layout()
            .bytecode_path(p.id)
            .to_str()
            .expect("path")
            .into();
        p.pin_path = writer
            .layout()
            .program_pin_path(p.id)
            .to_str()
            .expect("path")
            .into();
        p.map_path = writer
            .layout()
            .map_directory_path(p.id)
            .to_str()
            .expect("path")
            .into();
        let result = summary(&p);

        // Publish both facts together under the same state lock.
        s.program = Some(p);
        s.map_set = true;
        s.generation += 1;
        Ok(result)
    }
}

impl UnloadStore for Memory {
    type ProgramReceipt = ProgramReceipt;
    type MapSetReceipt = MapSetReceipt;

    fn observe_unload(
        &self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<UnloadObservation<Self>, Error> {
        self.authority(writer)?;
        let mut s = self.state.lock().expect("state");
        s.enter("observe")?;
        let Some(p) = &s.program else { return Ok(None) };
        if p.id != id {
            return Ok(None);
        }
        if !p.links.is_empty() || p.map_set != p.id || !matches!(p.spec, ProgramSpec::Tracepoint(_))
        {
            return Err(error(ErrorKind::Unsupported, "unsupported relationships"));
        }
        Ok(Some((
            ProgramReceipt(self.receipt(s.generation)),
            MapSetReceipt(self.receipt(s.generation)),
        )))
    }

    fn delete_program(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: ProgramReceipt,
    ) -> Result<(), EffectFailure<ProgramReceipt, Error>> {
        let result = (|| {
            let mut s = self.state.lock().expect("state");
            s.enter("delete program")?;
            self.validate(writer, &receipt.0, &s)?;
            if s.program.is_none() {
                return Err(error(ErrorKind::InvalidData, "program disappeared"));
            }
            s.program = None;
            Ok(())
        })();
        result.map_err(|cause| EffectFailure {
            cause,
            remaining: receipt,
        })
    }

    fn delete_map_set(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: MapSetReceipt,
    ) -> Result<(), EffectFailure<MapSetReceipt, Error>> {
        let result = (|| {
            let mut s = self.state.lock().expect("state");
            s.enter("delete map set")?;
            self.validate(writer, &receipt.0, &s)?;
            if s.program.is_some() || !s.map_set {
                return Err(error(
                    ErrorKind::InvalidData,
                    "map set still used or absent",
                ));
            }
            s.map_set = false;
            Ok(())
        })();
        result.map_err(|cause| EffectFailure {
            cause,
            remaining: receipt,
        })
    }
}
