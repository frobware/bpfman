//! The only translation between SQLite operations and portable store contracts.

use crate::{Backend, Store};
use bpfman_core::{EffectFailure, StoreObservation, StoreOpenPlan, plan_store_open};
use bpfman_fs::{RuntimeDirectory, RuntimeWriter};
use bpfman_model::{StoredProgram, StoredProgramSummary};
use bpfman_store::{
    CommitLoad, Error, LoadRecord, OpenStore, ProgramReader, UnloadObservation, UnloadStore,
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
    fn validate_map_sharing(&mut self) -> Result<(), Error> {
        self.validate()
    }

    fn read_map_sets(&mut self) -> Result<Vec<NonZeroU32>, Error> {
        self.reader
            .read(|connection| {
                let tx = connection.transaction()?;
                crate::open::require_supported(crate::open::schema_version(&tx)?)?;
                crate::queries::map_sets(&tx)?
                    .into_iter()
                    .map(|row| {
                        let id = row.id;
                        u32::try_from(id).ok().and_then(NonZeroU32::new).ok_or(
                            crate::error::Failure::InvalidRecord {
                                id,
                                reason: "invalid map-set ID".into(),
                            },
                        )
                    })
                    .collect::<Result<Vec<_>, crate::error::Failure>>()
            })
            .map_err(crate::Error::from)
            .map_err(Into::into)
    }

    fn read_shared_maps(
        &mut self,
    ) -> Result<std::collections::BTreeMap<String, Vec<NonZeroU32>>, Error> {
        self.reader
            .read(|connection| {
                let tx = connection.transaction()?;
                crate::open::require_supported(crate::open::schema_version(&tx)?)?;
                let mut maps = std::collections::BTreeMap::<String, Vec<NonZeroU32>>::new();
                for row in crate::queries::shared_maps(&tx)? {
                    let name = row.name;
                    let raw = row.program;
                    let id = u32::try_from(raw).ok().and_then(NonZeroU32::new).ok_or(
                        crate::error::Failure::InvalidRecord {
                            id: raw,
                            reason: "invalid shared-map user".into(),
                        },
                    )?;
                    if name.is_empty()
                        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    {
                        return Err(crate::error::Failure::InvalidRecord {
                            id: raw,
                            reason: "invalid shared map name".into(),
                        });
                    }
                    maps.entry(name).or_default().push(id);
                }
                Ok(maps)
            })
            .map_err(crate::Error::from)
            .map_err(Into::into)
    }

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
    fn commit_programs(
        &self,
        writer: &RuntimeWriter<'_>,
        records: &[LoadRecord<'_>],
    ) -> Result<(), Error> {
        crate::write::persist_batch(writer, records).map_err(Into::into)
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

    #[tracing::instrument(name = "store.retire_map_set", level = "debug", skip_all)]
    fn retire_map_set(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::MapSetReceipt,
    ) -> Result<(), EffectFailure<Self::MapSetReceipt, Error>> {
        crate::unload::retire_map_set(writer, receipt).map_err(failure)
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
