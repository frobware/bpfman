//! Existing readers bypass the writer lock; missing state is created under it.

use crate::{
    ActiveStore, Error,
    error::{filesystem_error, store_error},
};
use bpfman_core::EffectFailure;
use bpfman_fs::{RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use bpfman_lock::AcquireOptions;
use bpfman_store::{
    CommitLoad, OpenStore, ProgramReader, TracepointRecord, UnloadObservation, UnloadStore,
};
use std::num::NonZeroU32;
use std::time::Duration;

fn invalid(message: &'static str) -> bpfman_store::Error {
    bpfman_store::Error::new(
        bpfman_store::ErrorKind::InvalidData,
        std::io::Error::other(message),
    )
}

impl<S: OpenStore> ActiveStore<S> {
    pub(super) fn reader(&self) -> S::Reader {
        self.reader.clone()
    }

    /// Open the selected backend once at startup, creating state only if absent.
    pub fn open(backend: S, layout: &RuntimeLayout, timeout: Duration) -> Result<Self, Error> {
        Self::open_with_cancellation(backend, layout, timeout, &crate::Cancellation::new())
    }

    /// Open a store with cancellable admission, including initialization lock waiting.
    /// Once initialization begins, its atomic publication is allowed to finish.
    #[tracing::instrument(name = "store.open", level = "debug", skip_all, fields(runtime = %layout.root().display()), err)]
    pub fn open_with_cancellation(
        backend: S,
        layout: &RuntimeLayout,
        timeout: Duration,
        cancellation: &crate::Cancellation,
    ) -> Result<Self, Error> {
        cancellation.check()?;
        let runtime = match RuntimeDirectory::open_existing(layout.clone())
            .map_err(filesystem_error)?
        {
            Some(runtime) => runtime,
            None => RuntimeDirectory::open_or_create(layout.clone()).map_err(filesystem_error)?,
        };

        cancellation.check()?;

        let reader = match backend.open_reader(&runtime).map_err(store_error)? {
            Some(reader) => reader,
            None => runtime
                .with_writer(
                    AcquireOptions {
                        timeout,
                        cancelled: Some(cancellation.flag()),
                    },
                    |writer| {
                        cancellation.check()?;
                        backend.open(&writer).map_err(store_error)
                    },
                )
                .map_err(filesystem_error)??,
        };

        cancellation.check()?;

        Ok(Self {
            backend,
            reader,
            runtime,
        })
    }
}

impl<S: OpenStore> ActiveStore<S> {
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
        let mut reader = self.reader();
        reader.validate()?;

        Ok(Some(reader))
    }

    fn open(&self, writer: &RuntimeWriter<'_>) -> Result<Self::Reader, bpfman_store::Error> {
        self.check_writer(writer)?;
        let mut reader = self.reader();
        reader.validate()?;

        Ok(reader)
    }
}

impl<S: OpenStore + CommitLoad> CommitLoad for ActiveStore<S> {
    fn commit_tracepoints(
        &self,
        writer: &RuntimeWriter<'_>,
        records: &[TracepointRecord<'_>],
    ) -> Result<(), bpfman_store::Error> {
        self.check_writer(writer)?;
        self.backend.commit_tracepoints(writer, records)
    }
}

impl<S: OpenStore + UnloadStore> UnloadStore for ActiveStore<S> {
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

impl<S: OpenStore + bpfman_store::LinkStore> bpfman_store::LinkStore for ActiveStore<S> {
    type LinkReceipt = S::LinkReceipt;

    fn create_pending_tracepoint(
        &self,
        writer: &RuntimeWriter<'_>,
        request: bpfman_store::PendingTracepoint<'_>,
    ) -> Result<(bpfman_model::StoredLink, Self::LinkReceipt), bpfman_store::Error> {
        self.check_writer(writer)?;
        self.backend.create_pending_tracepoint(writer, request)
    }

    fn finalise_link(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::LinkReceipt,
        kernel_id: NonZeroU32,
    ) -> Result<bpfman_model::StoredLink, EffectFailure<Self::LinkReceipt, bpfman_store::Error>>
    {
        if let Err(cause) = self.check_writer(writer) {
            return Err(EffectFailure {
                remaining: receipt,
                cause,
            });
        }

        self.backend.finalise_link(writer, receipt, kernel_id)
    }

    fn observe_link(
        &self,
        writer: &RuntimeWriter<'_>,
        id: std::num::NonZeroU64,
    ) -> Result<bpfman_store::LinkObservation<Self>, bpfman_store::Error> {
        self.check_writer(writer)?;
        self.backend.observe_link(writer, id)
    }

    fn delete_link(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::LinkReceipt,
    ) -> Result<(), EffectFailure<Self::LinkReceipt, bpfman_store::Error>> {
        if let Err(cause) = self.check_writer(writer) {
            return Err(EffectFailure {
                remaining: receipt,
                cause,
            });
        }

        self.backend.delete_link(writer, receipt)
    }
}

#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
