//! Open the selected backend under runtime writer authority.
use crate::{
    Error,
    error::{filesystem_error, store_error},
};
use bpfman_fs::{RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use bpfman_lock::AcquireOptions;
use bpfman_store::OpenStore;
use std::time::Duration;

pub(super) fn open_or_create_store<S: OpenStore>(
    store: &S,
    layout: &RuntimeLayout,
    timeout: Duration,
) -> Result<S::Reader, Error> {
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).map_err(filesystem_error)?;
    runtime
        .with_writer(
            AcquireOptions {
                timeout,
                cancelled: None,
            },
            |writer| open_store(store, &writer),
        )
        .map_err(filesystem_error)?
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
