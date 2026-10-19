use super::Effects;
use crate::{
    ActiveStore, Bpfman, Cancellation, LinkCause, LinkError, UprobeAttach, link_error::Cause,
};
use bpfman_core::EffectFailure;
use bpfman_fs::RuntimeWriter;
use bpfman_model::{ProgramSpec, StoredLink, UprobeLink};
use bpfman_store::{LinkStore, OpenStore, ProgramReader};
use std::num::NonZeroU64;

struct Adapter<'a, S: OpenStore, K>(&'a ActiveStore<S>, &'a K, &'a Cancellation);
struct Prepared<P> {
    program: P,
    target: UprobeLink,
}

impl<S: OpenStore + LinkStore, K: bpfman_kernel::UprobeLinks> Effects for Adapter<'_, S, K> {
    type Request = UprobeAttach;
    type Prepared = Prepared<K::PreparedUprobe>;
    type Live = K::LiveTracepoint;
    type Pin = K::LinkPin;
    type Receipt = S::LinkReceipt;

    fn prepare(
        &mut self,
        writer: &RuntimeWriter<'_>,
        request: &UprobeAttach,
    ) -> Result<Self::Prepared, LinkCause> {
        let mut reader = self.0.open(writer)?;
        let record = reader
            .read_records()?
            .into_iter()
            .find(|r| r.id == request.program_id)
            .ok_or(Cause::NotFound)?;
        let retprobe = match record.spec {
            ProgramSpec::Uprobe(_) => false,
            ProgramSpec::Uretprobe(_) => true,
            _ => {
                return Err(Cause::ProgramMismatch {
                    id: record.id,
                    actual: record.spec.kind(),
                    requested: bpfman_model::ProgramType::Uprobe,
                }
                .into());
            }
        };
        if writer.layout().program_pin_path(record.id).to_str() != Some(record.pin_path.as_str()) {
            return Err(Cause::Invalid("program pin differs from canonical runtime layout").into());
        }
        let target = request.target.clone().into_link(retprobe);
        let program = self
            .1
            .prepare_uprobe_with_cancellation(writer, request.program_id, &target, self.2.flag())
            .map_err(|error| -> LinkCause {
                if self.2.is_cancelled() {
                    Cause::Cancelled.into()
                } else {
                    error.into()
                }
            })?;

        Ok(Prepared { program, target })
    }

    fn create(
        &mut self,
        writer: &RuntimeWriter<'_>,
        request: &UprobeAttach,
        created: &str,
        prepared: &Self::Prepared,
    ) -> Result<(StoredLink, Self::Receipt), LinkCause> {
        self.0
            .create_pending_uprobe(
                writer,
                bpfman_store::PendingUprobe {
                    program_id: request.program_id,
                    target: &prepared.target,
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
        _request: &UprobeAttach,
    ) -> Result<Self::Live, LinkCause> {
        self.1
            .attach_uprobe(writer, prepared.program, &prepared.target)
            .map_err(Into::into)
    }

    fn pin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        live: Self::Live,
        id: NonZeroU64,
    ) -> Result<Self::Pin, EffectFailure<Option<Self::Pin>, LinkCause>> {
        super::real::Adapter(self.0, self.1).pin(writer, live, id)
    }

    fn finalise(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::Receipt,
        pin: &Self::Pin,
    ) -> Result<StoredLink, EffectFailure<Self::Receipt, LinkCause>> {
        super::real::Adapter(self.0, self.1).finalise(writer, receipt, pin)
    }

    fn observe(
        &mut self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU64,
    ) -> Result<(StoredLink, Self::Receipt), LinkCause> {
        super::real::Adapter(self.0, self.1).observe(writer, id)
    }

    fn observe_pin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        record: &StoredLink,
    ) -> Result<Option<Self::Pin>, LinkCause> {
        super::real::Adapter(self.0, self.1).observe_pin(writer, record)
    }

    fn release(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::Live,
    ) -> Result<(), EffectFailure<Self::Live, LinkCause>> {
        super::real::Adapter(self.0, self.1).release(writer, receipt)
    }

    fn unpin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::Pin,
    ) -> Result<(), EffectFailure<Self::Pin, LinkCause>> {
        super::real::Adapter(self.0, self.1).unpin(writer, receipt)
    }

    fn delete(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::Receipt,
    ) -> Result<(), EffectFailure<Self::Receipt, LinkCause>> {
        super::real::Adapter(self.0, self.1).delete(writer, receipt)
    }
}

impl<S: OpenStore + LinkStore, K: bpfman_kernel::UprobeLinks> Bpfman<S, K> {
    /// Attach an entry or return probe using the standalone pending-intent protocol.
    pub fn attach_uprobe(&self, request: UprobeAttach) -> Result<StoredLink, LinkError<S, K>> {
        self.attach_uprobe_with_cancellation(request, &Cancellation::new())
    }

    /// Check cancellation between forward effects and retain unresolved cleanup ownership.
    #[tracing::instrument(name = "link.attach_uprobe", level = "debug", skip_all, err)]
    pub fn attach_uprobe_with_cancellation(
        &self,
        request: UprobeAttach,
        cancellation: &Cancellation,
    ) -> Result<StoredLink, LinkError<S, K>> {
        self.attach_standalone(
            request,
            cancellation,
            &mut Adapter(&self.store, &self.kernel, cancellation),
        )
    }
}
