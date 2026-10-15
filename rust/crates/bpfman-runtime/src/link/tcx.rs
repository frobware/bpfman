use super::Effects;
use crate::{
    ActiveStore, Bpfman, Cancellation, LinkCause, LinkError, TcxAttach, link_error::Cause,
};
use bpfman_core::EffectFailure;
use bpfman_fs::RuntimeWriter;
use bpfman_model::{LinkState, ProgramSpec, StoredLink, TcxLink, TcxOrder};
use bpfman_store::{LinkReader, LinkStore, OpenStore, ProgramReader};
use std::num::NonZeroU64;

struct Adapter<'a, S: OpenStore, K>(&'a ActiveStore<S>, &'a K);
struct Prepared<P> {
    target: TcxLink,
    program: P,
    order: TcxOrder,
}

fn failure<T>(error: EffectFailure<T, impl Into<LinkCause>>) -> EffectFailure<T, LinkCause> {
    EffectFailure {
        remaining: error.remaining,
        cause: error.cause.into(),
    }
}

impl<S: OpenStore + LinkStore, K: bpfman_kernel::TcxLinks + bpfman_kernel::LinkObservations> Effects
    for Adapter<'_, S, K>
where
    S::Reader: LinkReader,
{
    type Request = TcxAttach;
    type Prepared = Prepared<K::PreparedTcx>;
    type Live = K::LiveTracepoint;
    type Pin = K::LinkPin;
    type Receipt = S::LinkReceipt;

    fn prepare(
        &mut self,
        w: &RuntimeWriter<'_>,
        request: &TcxAttach,
    ) -> Result<Self::Prepared, LinkCause> {
        if request.priority > i32::MAX as u32 {
            return Err(Cause::Invalid("invalid TCX priority").into());
        }
        let mut reader = self.0.open(w)?;
        let program = reader
            .read_records()?
            .into_iter()
            .find(|p| p.id == request.program_id)
            .ok_or(Cause::NotFound)?;
        if !matches!(program.spec, ProgramSpec::Tcx(_)) {
            return Err(Cause::Invalid("program is not a tcx program").into());
        }
        if w.layout().program_pin_path(program.id).to_str() != Some(program.pin_path.as_str()) {
            return Err(Cause::Invalid("program pin differs from runtime layout").into());
        }
        let (target, prepared) = self.1.prepare_tcx(
            w,
            program.id,
            &request.interface,
            &request.netns,
            request.direction,
            request.priority,
        )?;
        let mut members = Vec::new();
        for record in reader.read_links()? {
            let bpfman_model::LinkDetails::Tcx(d) = &record.details else {
                continue;
            };
            if d.key != target.key {
                continue;
            }
            if record.program_id == request.program_id {
                return Err(Cause::AlreadyAttached(record.id).into());
            }
            let LinkState::Attached { kernel_id } = record.state else {
                return Err(
                    Cause::Invalid("pending TCX intent requires cleanup before attach").into(),
                );
            };
            match self.1.tcx_link(kernel_id) {
                Err(e) if e.kind() == bpfman_kernel::ErrorKind::Missing => continue,
                Err(e) => return Err(e.into()),
                Ok(link) => {
                    if link.program_id != record.program_id {
                        return Err(Cause::Invalid("TCX anchor program changed").into());
                    }
                    match link.details {
                        bpfman_model::KernelLinkDetails::Tcx { ifindex: 0, .. } => continue,
                        bpfman_model::KernelLinkDetails::Tcx { ifindex, direction }
                            if ifindex == target.key.ifindex.get()
                                && direction == target.key.direction => {}
                        _ => return Err(Cause::Invalid("TCX anchor hook changed").into()),
                    }
                }
            }
            members.push((d.priority, record.id, record.program_id));
        }
        members.sort_by_key(|&(priority, id, _)| (priority, id));
        let anchors: Vec<_> = members.into_iter().map(|(p, _, id)| (p, id)).collect();
        Ok(Prepared {
            target,
            program: prepared,
            order: bpfman_model::tcx_order(&anchors, request.priority),
        })
    }

    fn create(
        &mut self,
        w: &RuntimeWriter<'_>,
        request: &TcxAttach,
        created: &str,
        prepared: &Self::Prepared,
    ) -> Result<(StoredLink, Self::Receipt), LinkCause> {
        self.0
            .create_pending_tcx(
                w,
                bpfman_store::PendingTcx {
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
        w: &RuntimeWriter<'_>,
        prepared: Self::Prepared,
        _request: &TcxAttach,
    ) -> Result<Self::Live, LinkCause> {
        self.1
            .attach_tcx(w, prepared.program, prepared.order)
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
        let path = match &record.details {
            bpfman_model::LinkDetails::Tracepoint(_)
            | bpfman_model::LinkDetails::Tracing(_)
            | bpfman_model::LinkDetails::Kprobe(_)
            | bpfman_model::LinkDetails::Uprobe(_) => writer.layout().link_pin_path(record.id),
            bpfman_model::LinkDetails::Tcx(d) => {
                writer.layout().tcx_link_path(d.key, record.program_id)
            }
            _ => return Err(Cause::Unsupported.into()),
        };
        if path.to_str() != Some(record.pin_path.as_str()) {
            return Err(Cause::Invalid("link pin differs from canonical runtime layout").into());
        }

        let kernel = match record.state {
            LinkState::Pending => None,
            LinkState::Attached { kernel_id } => Some(kernel_id),
        };
        match &record.details {
            bpfman_model::LinkDetails::Tracing(d) => {
                self.1
                    .observe_tracing_pin(writer, record.id, record.program_id, kernel, d.kind)
            }
            bpfman_model::LinkDetails::Tcx(d) => {
                self.1.observe_tcx_pin(writer, d, record.program_id, kernel)
            }
            _ => self
                .1
                .observe_link_pin(writer, record.id, record.program_id, kernel),
        }
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

impl<S: OpenStore + LinkStore, K: bpfman_kernel::TcxLinks + bpfman_kernel::LinkObservations>
    Bpfman<S, K>
where
    S::Reader: LinkReader,
{
    /// Attach native TCX using the shared pending-intent and compensation protocol.
    pub fn attach_tcx(&self, request: TcxAttach) -> Result<StoredLink, LinkError<S, K>> {
        self.attach_tcx_with_cancellation(request, &Cancellation::new())
    }

    /// Observe cancellation between effects, retaining every acquired cleanup receipt.
    #[tracing::instrument(name = "link.attach_tcx", level = "debug", skip_all, err)]
    pub fn attach_tcx_with_cancellation(
        &self,
        request: TcxAttach,
        cancellation: &Cancellation,
    ) -> Result<StoredLink, LinkError<S, K>> {
        self.attach_standalone(
            request,
            cancellation,
            &mut Adapter(&self.store, &self.kernel),
        )
    }
}
