use super::Effects;
use crate::{
    ActiveStore, Bpfman, Cancellation, LinkCause, LinkError, TracingAttach, link_error::Cause,
};
use bpfman_core::EffectFailure;
use bpfman_fs::RuntimeWriter;
use bpfman_model::{ProgramSpec, StoredLink, TracingLink};
use bpfman_store::{LinkStore, OpenStore, ProgramReader};
use std::num::NonZeroU64;

struct Adapter<'a, S: OpenStore, K>(&'a ActiveStore<S>, &'a K);
struct Prepared<P> {
    program: P,
    target: TracingLink,
}

impl<S: OpenStore + LinkStore, K: bpfman_kernel::TracingLinks> Effects for Adapter<'_, S, K> {
    type Request = TracingAttach;
    type Prepared = Prepared<K::PreparedTracing>;
    type Live = K::LiveTracepoint;
    type Pin = K::LinkPin;
    type Receipt = S::LinkReceipt;

    fn prepare(
        &mut self,
        writer: &RuntimeWriter<'_>,
        request: &TracingAttach,
    ) -> Result<Self::Prepared, LinkCause> {
        let mut reader = self.0.open(writer)?;
        let record = reader
            .read_records()?
            .into_iter()
            .find(|r| r.id == request.program_id)
            .ok_or(Cause::NotFound)?;
        let target = match &record.spec {
            ProgramSpec::Fentry { target, .. }
                if request.kind == bpfman_model::TracingKind::Fentry =>
            {
                TracingLink {
                    function: target.clone(),
                    kind: request.kind,
                }
            }
            ProgramSpec::Fexit { target, .. }
                if request.kind == bpfman_model::TracingKind::Fexit =>
            {
                TracingLink {
                    function: target.clone(),
                    kind: request.kind,
                }
            }
            ProgramSpec::Lsm { hook, .. } if request.kind == bpfman_model::TracingKind::Lsm => {
                TracingLink {
                    function: hook.clone(),
                    kind: request.kind,
                }
            }
            _ => {
                return Err(
                    Cause::Invalid("program is not the requested BTF attachment family").into(),
                );
            }
        };
        if writer.layout().program_pin_path(record.id).to_str() != Some(record.pin_path.as_str()) {
            return Err(Cause::Invalid("program pin differs from canonical runtime layout").into());
        }
        let program = self
            .1
            .prepare_tracing(writer, request.program_id, &target)?;
        Ok(Prepared { program, target })
    }

    fn create(
        &mut self,
        writer: &RuntimeWriter<'_>,
        request: &TracingAttach,
        created: &str,
        prepared: &Self::Prepared,
    ) -> Result<(StoredLink, Self::Receipt), LinkCause> {
        self.0
            .create_pending_tracing(
                writer,
                bpfman_store::PendingTracing {
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
        _request: &TracingAttach,
    ) -> Result<Self::Live, LinkCause> {
        self.1
            .attach_tracing(writer, prepared.program, &prepared.target)
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

impl<S: OpenStore + LinkStore, K: bpfman_kernel::TracingLinks> Bpfman<S, K> {
    /// Attach function tracing or an LSM hook using the standalone pending-intent protocol.
    pub fn attach_tracing(&self, request: TracingAttach) -> Result<StoredLink, LinkError<S, K>> {
        self.attach_tracing_with_cancellation(request, &Cancellation::new())
    }

    /// Check cancellation between forward effects and retain unresolved cleanup ownership.
    #[tracing::instrument(name = "link.attach_tracing", level = "debug", skip_all, err)]
    pub fn attach_tracing_with_cancellation(
        &self,
        request: TracingAttach,
        cancellation: &Cancellation,
    ) -> Result<StoredLink, LinkError<S, K>> {
        self.attach_standalone(
            request,
            cancellation,
            &mut Adapter(&self.store, &self.kernel),
        )
    }
}
