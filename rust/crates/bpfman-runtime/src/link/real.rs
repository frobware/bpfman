use super::Effects;
use crate::{ActiveStore, LinkCause, TracepointAttach, link_error::Cause};
use bpfman_core::EffectFailure;
use bpfman_fs::RuntimeWriter;
use bpfman_model::{LinkState, ProgramSpec, StoredLink};
use bpfman_store::{LinkStore, OpenStore, PendingTracepoint, ProgramReader};
use std::num::{NonZeroU32, NonZeroU64};

pub(super) struct Adapter<'a, S: OpenStore, K>(pub(super) &'a ActiveStore<S>, pub(super) &'a K);

fn failure<T>(error: EffectFailure<T, impl Into<LinkCause>>) -> EffectFailure<T, LinkCause> {
    EffectFailure {
        remaining: error.remaining,
        cause: error.cause.into(),
    }
}

impl<S: OpenStore + LinkStore, K: bpfman_kernel::TracepointLinks> Effects for Adapter<'_, S, K> {
    type Prepared = K::PreparedTracepoint;
    type Live = K::LiveTracepoint;
    type Pin = K::LinkPin;
    type Receipt = S::LinkReceipt;

    fn prepare(
        &mut self,
        writer: &RuntimeWriter<'_>,
        program: NonZeroU32,
    ) -> Result<Self::Prepared, LinkCause> {
        let mut reader = self.0.open(writer)?;
        let record = reader
            .read_records()?
            .into_iter()
            .find(|record| record.id == program)
            .ok_or(Cause::NotFound)?;

        if !matches!(record.spec, ProgramSpec::Tracepoint(_)) {
            return Err(Cause::Unsupported.into());
        }

        if writer.layout().program_pin_path(program).to_str() != Some(record.pin_path.as_str()) {
            return Err(Cause::Invalid("program pin differs from canonical runtime layout").into());
        }

        self.1
            .prepare_tracepoint(writer, program)
            .map_err(Into::into)
    }

    fn create(
        &mut self,
        writer: &RuntimeWriter<'_>,
        request: &TracepointAttach,
        created: &str,
    ) -> Result<(StoredLink, Self::Receipt), LinkCause> {
        self.0
            .create_pending_tracepoint(
                writer,
                PendingTracepoint {
                    program_id: request.program_id,
                    target: &request.target,
                    metadata: &request.metadata,
                    created_at: created,
                },
            )
            .map_err(Into::into)
    }

    fn attach(
        &mut self,
        writer: &RuntimeWriter<'_>,
        prepared: Self::Prepared,
        request: &TracepointAttach,
    ) -> Result<Self::Live, LinkCause> {
        self.1
            .attach_tracepoint(writer, prepared, &request.target)
            .map_err(Into::into)
    }

    fn pin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        live: Self::Live,
        id: NonZeroU64,
    ) -> Result<Self::Pin, EffectFailure<Option<Self::Pin>, LinkCause>> {
        self.1.pin_tracepoint(writer, live, id).map_err(failure)
    }

    fn finalise(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::Receipt,
        pin: &Self::Pin,
    ) -> Result<StoredLink, EffectFailure<Self::Receipt, LinkCause>> {
        self.0
            .finalise_link(writer, receipt, K::link_id(pin))
            .map_err(failure)
    }

    fn observe(
        &mut self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU64,
    ) -> Result<(StoredLink, Self::Receipt), LinkCause> {
        self.0.open(writer)?;
        self.0
            .observe_link(writer, id)?
            .ok_or_else(|| Cause::NotFound.into())
    }

    fn observe_pin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        record: &StoredLink,
    ) -> Result<Option<Self::Pin>, LinkCause> {
        if writer.layout().link_pin_path(record.id).to_str() != Some(record.pin_path.as_str()) {
            return Err(Cause::Invalid("link pin differs from canonical runtime layout").into());
        }

        let kernel = match record.state {
            LinkState::Pending => None,
            LinkState::Attached { kernel_id } => Some(kernel_id),
        };
        self.1
            .observe_link_pin(writer, record.id, record.program_id, kernel)
            .map_err(Into::into)
    }

    fn release(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::Live,
    ) -> Result<(), EffectFailure<Self::Live, LinkCause>> {
        self.1.release_tracepoint(writer, receipt).map_err(failure)
    }

    fn unpin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::Pin,
    ) -> Result<(), EffectFailure<Self::Pin, LinkCause>> {
        self.1.remove_link(writer, receipt).map_err(failure)
    }

    fn delete(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::Receipt,
    ) -> Result<(), EffectFailure<Self::Receipt, LinkCause>> {
        self.0.delete_link(writer, receipt).map_err(failure)
    }
}
