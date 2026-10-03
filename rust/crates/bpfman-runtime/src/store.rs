//! Existing readers bypass the writer lock; missing state is created under it.

use crate::{
    ActiveStore, Error,
    error::{filesystem_error, store_error},
};
use bpfman_core::EffectFailure;
use bpfman_fs::{RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use bpfman_lock::AcquireOptions;
use bpfman_model::StoredProgramSummary;
use bpfman_store::{CommitLoad, OpenStore, TracepointRecord, UnloadObservation, UnloadStore};
use std::num::NonZeroU32;
use std::time::Duration;

fn invalid(message: &'static str) -> bpfman_store::Error {
    bpfman_store::Error::new(
        bpfman_store::ErrorKind::InvalidData,
        std::io::Error::other(message),
    )
}

impl<S: OpenStore> ActiveStore<S> {
    pub(super) fn reader(&self) -> Result<S::Reader, bpfman_store::Error> {
        self.backend
            .open_reader(&self.runtime)?
            .ok_or_else(|| invalid("active store disappeared"))
    }

    /// Open the selected backend once at startup, creating state only if absent.
    #[tracing::instrument(name = "store.open", level = "debug", skip_all, fields(runtime = %layout.root().display()), err)]
    pub fn open(backend: S, layout: &RuntimeLayout, timeout: Duration) -> Result<Self, Error> {
        let runtime = match RuntimeDirectory::open_existing(layout.clone())
            .map_err(filesystem_error)?
        {
            Some(runtime) => runtime,
            None => RuntimeDirectory::open_or_create(layout.clone()).map_err(filesystem_error)?,
        };

        if backend
            .open_reader(&runtime)
            .map_err(store_error)?
            .is_none()
        {
            runtime
                .with_writer(
                    AcquireOptions {
                        timeout,
                        cancelled: None,
                    },
                    |writer| backend.open(&writer).map_err(store_error),
                )
                .map_err(filesystem_error)??;
        }

        Ok(Self { backend, runtime })
    }
}

impl<S> ActiveStore<S> {
    pub(super) fn runtime(&self) -> &RuntimeDirectory {
        &self.runtime
    }

    fn check_root(&self, identity: bpfman_fs::RuntimeIdentity) -> Result<(), bpfman_store::Error> {
        let root = self
            .runtime
            .identity()
            .map_err(|e| bpfman_store::Error::new(bpfman_store::ErrorKind::Unavailable, e))?;
        if root != identity {
            return Err(invalid("active store belongs to another runtime"));
        }

        Ok(())
    }

    fn check_writer(&self, writer: &RuntimeWriter<'_>) -> Result<(), bpfman_store::Error> {
        self.check_root(
            writer
                .identity()
                .map_err(|e| bpfman_store::Error::new(bpfman_store::ErrorKind::Unavailable, e))?,
        )
    }
}

impl<S: OpenStore> OpenStore for ActiveStore<S> {
    type Reader = S::Reader;

    fn open_reader(
        &self,
        runtime: &RuntimeDirectory,
    ) -> Result<Option<Self::Reader>, bpfman_store::Error> {
        self.check_root(
            runtime
                .identity()
                .map_err(|e| bpfman_store::Error::new(bpfman_store::ErrorKind::Unavailable, e))?,
        )?;
        self.reader().map(Some)
    }

    fn open(&self, writer: &RuntimeWriter<'_>) -> Result<Self::Reader, bpfman_store::Error> {
        self.check_writer(writer)?;
        self.reader()
    }
}

impl<S: CommitLoad> CommitLoad for ActiveStore<S> {
    fn commit_tracepoint(
        &self,
        writer: &RuntimeWriter<'_>,
        record: TracepointRecord<'_>,
    ) -> Result<StoredProgramSummary, bpfman_store::Error> {
        self.check_writer(writer)?;
        self.backend.commit_tracepoint(writer, record)
    }
}

impl<S: UnloadStore> UnloadStore for ActiveStore<S> {
    type ProgramReceipt = S::ProgramReceipt;
    type MapSetReceipt = S::MapSetReceipt;

    fn observe_unload(
        &self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<UnloadObservation<Self>, bpfman_store::Error> {
        self.check_writer(writer)?;
        self.backend.observe_unload(writer, id)
    }

    fn delete_program(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::ProgramReceipt,
    ) -> Result<(), EffectFailure<Self::ProgramReceipt, bpfman_store::Error>> {
        if let Err(cause) = self.check_writer(writer) {
            return Err(EffectFailure {
                remaining: receipt,
                cause,
            });
        }

        self.backend.delete_program(writer, receipt)
    }

    fn delete_map_set(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::MapSetReceipt,
    ) -> Result<(), EffectFailure<Self::MapSetReceipt, bpfman_store::Error>> {
        if let Err(cause) = self.check_writer(writer) {
            return Err(EffectFailure {
                remaining: receipt,
                cause,
            });
        }

        self.backend.delete_map_set(writer, receipt)
    }
}

pub(super) fn open_store<S: OpenStore>(
    store: &S,
    writer: &RuntimeWriter<'_>,
) -> Result<S::Reader, Error> {
    store.open(writer).map_err(store_error)
}

#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
