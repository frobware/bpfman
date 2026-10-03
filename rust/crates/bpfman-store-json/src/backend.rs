use crate::{Backend, MapSetReceipt, ProgramReceipt, Reader, error::Failure, state::State};
use bpfman_core::EffectFailure;
use bpfman_fs::{RuntimeDirectory, RuntimeWriter, StoreSnapshot};
use bpfman_model::{StoredProgram, StoredProgramSummary};
use bpfman_store::{
    CommitLoad, Error, OpenStore, ProgramReader, TracepointRecord, UnloadObservation, UnloadStore,
};
use std::num::NonZeroU32;

fn read(file: &StoreSnapshot) -> Result<(Vec<u8>, State), Failure> {
    let bytes = file.read()?.ok_or(Failure::Invalid("store disappeared"))?;
    let state = State::decode(&bytes)?;

    Ok((bytes, state))
}

#[tracing::instrument(name = "store.publish", level = "debug", skip_all, fields(programs = state.programs.len(), map_sets = state.map_sets.len()), err)]
fn publish(
    writer: &RuntimeWriter<'_>,
    file: &StoreSnapshot,
    previous: Option<&[u8]>,
    state: &State,
) -> Result<(), Failure> {
    let bytes = serde_json::to_vec_pretty(state)?;
    writer.publish_store_snapshot(file, previous, &bytes)?;

    Ok(())
}

impl OpenStore for Backend {
    type Reader = Reader;

    #[tracing::instrument(name = "store.open_reader", level = "debug", skip_all, err)]
    fn open_reader(&self, runtime: &RuntimeDirectory) -> Result<Option<Reader>, Error> {
        let Some(file) = runtime.open_store_snapshot().map_err(Failure::from)? else {
            return Ok(None);
        };
        let Some(bytes) = file.read().map_err(Failure::from)? else {
            return Ok(None);
        };
        State::decode(&bytes)?;

        Ok(Some(Reader {
            file,
            layout: runtime.layout().clone(),
        }))
    }

    #[tracing::instrument(name = "store.open_or_create", level = "debug", skip_all, err)]
    fn open(&self, writer: &RuntimeWriter<'_>) -> Result<Reader, Error> {
        let file = writer.open_store_snapshot().map_err(Failure::from)?;

        match file.read().map_err(Failure::from)? {
            Some(bytes) => {
                State::decode(&bytes)?;
            }
            None => {
                let _creation = tracing::debug_span!("store.create").entered();
                publish(writer, &file, None, &State::empty()?)?;
            }
        }

        Ok(Reader {
            file,
            layout: writer.layout().clone(),
        })
    }
}

impl ProgramReader for Reader {
    #[tracing::instrument(name = "store.read_summaries", level = "debug", skip_all, err)]
    fn read_programs(&mut self) -> Result<Vec<StoredProgramSummary>, Error> {
        let (_, state) = read(&self.file)?;

        Ok(state.programs.iter().map(|row| row.summary()).collect())
    }

    #[tracing::instrument(name = "store.snapshot", level = "debug", skip_all, err)]
    fn read_records(&mut self) -> Result<Vec<StoredProgram>, Error> {
        let (_, state) = read(&self.file)?;
        state
            .programs
            .iter()
            .map(|row| row.record(&self.layout).map_err(Into::into))
            .collect()
    }
}

impl CommitLoad for Backend {
    #[tracing::instrument(name = "store.commit", level = "debug", skip_all, err)]
    fn commit_tracepoint(
        &self,
        writer: &RuntimeWriter<'_>,
        record: TracepointRecord<'_>,
    ) -> Result<StoredProgramSummary, Error> {
        let file = writer.open_store_snapshot().map_err(Failure::from)?;
        let (previous, mut state) = read(&file)?;
        let summary = state.insert(record)?;

        // Ensure every record can be observed before transferring ownership.
        for row in &state.programs {
            row.record(writer.layout())?;
        }

        publish(writer, &file, Some(&previous), &state)?;

        Ok(summary)
    }
}

impl UnloadStore for Backend {
    type ProgramReceipt = ProgramReceipt;
    type MapSetReceipt = MapSetReceipt;

    #[tracing::instrument(name = "store.observe_unload", level = "debug", skip_all, err)]
    fn observe_unload(
        &self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<UnloadObservation<Self>, Error> {
        let file = writer.open_store_snapshot().map_err(Failure::from)?;
        let Some(bytes) = file.read().map_err(Failure::from)? else {
            return Ok(None);
        };

        let state = State::decode(&bytes)?;
        let Some(row) = state.programs.iter().find(|row| row.id == id) else {
            return Ok(None);
        };
        let map = state
            .map_sets
            .iter()
            .find(|map| map.id == id)
            .ok_or(Failure::Invalid("missing private map set"))?;
        let root = writer.identity().map_err(Failure::from)?;

        Ok(Some((
            ProgramReceipt {
                root,
                store: state.identity.clone(),
                row: row.clone(),
            },
            MapSetReceipt {
                root,
                store: state.identity.clone(),
                row: map.clone(),
            },
        )))
    }

    #[tracing::instrument(name = "store.delete_program", level = "debug", skip_all)]
    fn delete_program(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: ProgramReceipt,
    ) -> Result<(), EffectFailure<ProgramReceipt, Error>> {
        let result = (|| -> Result<(), Failure> {
            if writer.identity()? != receipt.root {
                return Err(Failure::Invalid("program belongs to another runtime"));
            }

            let file = writer.open_store_snapshot()?;
            let (previous, mut state) = read(&file)?;

            if state.identity != receipt.store {
                return Err(Failure::Invalid("store identity changed"));
            }

            let index = state
                .programs
                .iter()
                .position(|row| row == &receipt.row)
                .ok_or(Failure::Invalid("program changed since observation"))?;
            state.programs.remove(index);
            publish(writer, &file, Some(&previous), &state)
        })();

        result.map_err(|cause| EffectFailure {
            remaining: receipt,
            cause: cause.into(),
        })
    }

    #[tracing::instrument(name = "store.delete_map_set", level = "debug", skip_all)]
    fn delete_map_set(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: MapSetReceipt,
    ) -> Result<(), EffectFailure<MapSetReceipt, Error>> {
        let result = (|| -> Result<(), Failure> {
            if writer.identity()? != receipt.root {
                return Err(Failure::Invalid("map set belongs to another runtime"));
            }

            let file = writer.open_store_snapshot()?;
            let (previous, mut state) = read(&file)?;

            if state.identity != receipt.store
                || state.programs.iter().any(|row| row.id == receipt.row.id)
            {
                return Err(Failure::Invalid("map set changed or is still in use"));
            }

            let index = state
                .map_sets
                .iter()
                .position(|row| row == &receipt.row)
                .ok_or(Failure::Invalid("map set changed since observation"))?;
            state.map_sets.remove(index);
            publish(writer, &file, Some(&previous), &state)
        })();

        result.map_err(|cause| EffectFailure {
            remaining: receipt,
            cause: cause.into(),
        })
    }
}
