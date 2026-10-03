//! The only translation between SQLite operations and portable store contracts.

use crate::{Backend, Store};
use bpfman_core::{EffectFailure, StoreObservation, StoreOpenPlan, plan_store_open};
use bpfman_fs::{RuntimeDirectory, RuntimeWriter};
use bpfman_model::{StoredProgram, StoredProgramSummary};
use bpfman_store::{
    CommitLoad, Error, OpenStore, ProgramReader, TracepointRecord, UnloadObservation, UnloadStore,
};
use std::num::NonZeroU32;

impl OpenStore for Backend {
    type Reader = Store;

    #[tracing::instrument(name = "store.open_reader", level = "debug", skip_all, err)]
    fn open_reader(&self, runtime: &RuntimeDirectory) -> Result<Option<Store>, Error> {
        let store = Store::inspect(&runtime.layout().database_path())?;
        if let Some(store) = &store {
            crate::open::require_supported(store.schema_version()).map_err(crate::Error::from)?;
        }

        Ok(store)
    }

    #[tracing::instrument(name = "store.open_or_create", level = "debug", skip_all, err)]
    fn open(&self, writer: &RuntimeWriter<'_>) -> Result<Store, Error> {
        let path = writer.database_path();
        let observed = match Store::inspect(&path)? {
            None => StoreObservation::Missing,
            Some(store) => StoreObservation::Existing {
                version: store.schema_version(),
                evidence: store,
            },
        };

        match plan_store_open(observed, crate::SCHEMA_VERSION) {
            StoreOpenPlan::Create => {
                crate::create_if_missing(writer)?;
                Store::open(&path).map_err(Into::into)
            }
            StoreOpenPlan::UseExisting(store) => Ok(store),
            StoreOpenPlan::RejectIncompatible { found, .. } => {
                Err(crate::Error::from(crate::error::Failure::UnsupportedSchema { found }).into())
            }
        }
    }
}

impl ProgramReader for Store {
    #[tracing::instrument(name = "store.validate", level = "debug", skip_all, err)]
    fn validate(&mut self) -> Result<(), Error> {
        self.reader
            .read(|connection| {
                let tx = connection.transaction()?;
                crate::open::require_supported(crate::open::schema_version(&tx)?)
            })
            .map_err(crate::Error::from)
            .map_err(Into::into)
    }

    #[tracing::instrument(name = "store.read_summaries", level = "debug", skip_all, err)]
    fn read_programs(&mut self) -> Result<Vec<StoredProgramSummary>, Error> {
        Store::read_programs(self).map_err(Into::into)
    }

    #[tracing::instrument(name = "store.snapshot", level = "debug", skip_all, err)]
    fn read_records(&mut self) -> Result<Vec<StoredProgram>, Error> {
        Store::read_records(self).map_err(Into::into)
    }
}

impl CommitLoad for Backend {
    #[tracing::instrument(name = "store.commit", level = "debug", skip_all, err)]
    fn commit_tracepoint(
        &self,
        writer: &RuntimeWriter<'_>,
        record: TracepointRecord<'_>,
    ) -> Result<StoredProgramSummary, Error> {
        crate::persist_tracepoint(writer, record).map_err(Into::into)
    }
}

fn failure<R>(f: EffectFailure<R, crate::Error>) -> EffectFailure<R, Error> {
    EffectFailure {
        remaining: f.remaining,
        cause: f.cause.into(),
    }
}

impl UnloadStore for Backend {
    type ProgramReceipt = crate::ProgramRecord;
    type MapSetReceipt = crate::PrivateMapSet;

    #[tracing::instrument(name = "store.observe_unload", level = "debug", skip_all, err)]
    fn observe_unload(
        &self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<UnloadObservation<Self>, Error> {
        crate::observe_unload(writer, id)
            .map(|r| r.map(|r| r.into_parts()))
            .map_err(Into::into)
    }

    #[tracing::instrument(name = "store.delete_program", level = "debug", skip_all)]
    fn delete_program(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::ProgramReceipt,
    ) -> Result<(), EffectFailure<Self::ProgramReceipt, Error>> {
        crate::delete_unloaded_program(writer, receipt).map_err(failure)
    }

    #[tracing::instrument(name = "store.delete_map_set", level = "debug", skip_all)]
    fn delete_map_set(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::MapSetReceipt,
    ) -> Result<(), EffectFailure<Self::MapSetReceipt, Error>> {
        crate::delete_unused_map_set(writer, receipt).map_err(failure)
    }
}
