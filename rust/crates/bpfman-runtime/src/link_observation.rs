use crate::{Bpfman, Cancellation, LinkCause, link_error::Cause};
use bpfman_model::{LinkState, ObservedLink};
use bpfman_store::{LinkReader, OpenStore};
use std::num::NonZeroU64;

impl<S: OpenStore> Bpfman<S>
where
    S::Reader: LinkReader,
{
    /// Observe one stored standalone link without taking the writer lock.
    pub fn get_link(&self, id: NonZeroU64) -> Result<ObservedLink, LinkCause> {
        self.get_link_with_cancellation(id, &Cancellation::new())
    }

    /// Read intent, kernel identity and pin presence independently. Missing
    /// kernel state is an observation; denied inspection remains an error.
    #[tracing::instrument(name = "link.get", level = "debug", skip_all, fields(link_id = id.get()), err)]
    pub fn get_link_with_cancellation(
        &self,
        id: NonZeroU64,
        cancellation: &Cancellation,
    ) -> Result<ObservedLink, LinkCause> {
        let record = self
            .list_link_records_with_cancellation(cancellation)?
            .into_iter()
            .find(|r| r.id == id)
            .ok_or(Cause::NotFound)?;

        observe_record(self.store.runtime(), record, cancellation)
    }
}

pub(super) fn observe_record(
    runtime: &bpfman_fs::RuntimeDirectory,
    record: bpfman_model::StoredLink,
    cancellation: &Cancellation,
) -> Result<ObservedLink, LinkCause> {
    let (path, observer): (_, fn(_) -> _) = match &record.details {
        bpfman_model::LinkDetails::Tracepoint(_) => (
            runtime.layout().link_pin_path(record.id),
            bpfman_kernel::observe_tracepoint_link,
        ),
        bpfman_model::LinkDetails::Xdp(details) => (
            runtime
                .layout()
                .xdp_extension_path(details.key, details.revision),
            bpfman_kernel::observe_extension_link,
        ),
    };
    if path.to_str() != Some(record.pin_path.as_str()) {
        return Err(Cause::Invalid("link pin differs from canonical runtime layout").into());
    }

    let kernel_id = match record.state {
        LinkState::Pending => None,
        LinkState::Attached { kernel_id } => Some(kernel_id),
    };
    let kernel = match kernel_id.map(observer).transpose() {
        Ok(kernel) => kernel,
        Err(error) if error.kind() == bpfman_kernel::ErrorKind::Missing => None,
        Err(error) => return Err(error.into()),
    };
    let pin = match &record.details {
        bpfman_model::LinkDetails::Tracepoint(_) => runtime.read_link_pin(record.id)?,
        bpfman_model::LinkDetails::Xdp(details) => runtime.read_xdp_link_pin(details)?,
    };
    if kernel
        .as_ref()
        .is_some_and(|k| k.program_id != record.program_id)
        || pin.as_ref().is_some_and(|p| {
            p.program_id != record.program_id || kernel_id.is_some_and(|expected| p.id != expected)
        })
    {
        return Err(Cause::Invalid("observed link differs from stored identity").into());
    }
    if let (bpfman_model::LinkDetails::Xdp(expected), Some(observed)) = (&record.details, &kernel) {
        if !matches!(
            observed.details,
            bpfman_model::KernelLinkDetails::Tracing { target_obj_id, .. }
                if target_obj_id == expected.dispatcher_id.get()
        ) {
            return Err(Cause::Invalid("extension target differs from stored dispatcher").into());
        }
    }
    cancellation.check().map_err(|_| Cause::Cancelled)?;

    Ok(ObservedLink {
        record,
        kernel,
        pin_present: pin.is_some(),
    })
}
