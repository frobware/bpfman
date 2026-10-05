use crate::{
    Backend, LinkReceipt, Reader,
    backend::{publish, read},
    error::Failure,
    state::{Link, LinkProgress},
};
use bpfman_core::EffectFailure;
use bpfman_fs::RuntimeWriter;
use bpfman_model::StoredLink;
use bpfman_store::{Error, LinkObservation, LinkReader, LinkStore, PendingTracepoint};
use std::num::{NonZeroU32, NonZeroU64};

impl LinkReader for Reader {
    #[tracing::instrument(name = "store.read_links", level = "debug", skip_all, err)]
    fn read_links(&mut self) -> Result<Vec<StoredLink>, Error> {
        let (_, state) = read(&self.file)?;
        let mut records = state
            .links
            .iter()
            .map(|link| link.record(&self.layout))
            .collect::<Result<Vec<_>, _>>()?;
        records.extend(
            state
                .xdp
                .iter()
                .map(|row| state.xdp_snapshot(row, &self.layout).map(|s| s.member))
                .collect::<Result<Vec<_>, _>>()?,
        );
        records.sort_by_key(|link| link.id);

        Ok(records)
    }
}

fn check(
    writer: &RuntimeWriter<'_>,
    state: &crate::state::State,
    receipt: &LinkReceipt,
) -> Result<usize, Failure> {
    if writer.identity()? != receipt.root || state.identity != receipt.store {
        return Err(Failure::Invalid("link belongs to another runtime or store"));
    }

    state
        .links
        .iter()
        .position(|row| row == &receipt.row)
        .ok_or(Failure::Invalid("link changed since observation"))
}

impl LinkStore for Backend {
    type LinkReceipt = LinkReceipt;

    #[tracing::instrument(name = "store.create_pending_link", level = "debug", skip_all, err)]
    fn create_pending_tracepoint(
        &self,
        writer: &RuntimeWriter<'_>,
        request: PendingTracepoint<'_>,
    ) -> Result<(StoredLink, LinkReceipt), Error> {
        let file = writer.open_store_snapshot().map_err(Failure::from)?;
        let (previous, mut state) = read(&file)?;

        if state.version < 2 {
            return Err(Failure::LinkVersion.into());
        }

        if !state.programs.iter().any(|program| {
            program.id == request.program_id && program.kind == crate::state::Kind::Tracepoint
        }) {
            return Err(Failure::Invalid("missing tracepoint program").into());
        }

        let id = NonZeroU64::new(state.next_link_id)
            .filter(|id| id.get() <= i64::MAX as u64)
            .ok_or(Failure::Invalid("link ID exhausted"))?;
        state.next_link_id += 1;
        let row = Link {
            id,
            program_id: request.program_id,
            target: request.target.to_string(),
            state: LinkProgress::Pending,
            metadata: request.metadata.clone(),
            created_at: request.created_at.into(),
        };
        let record = row.record(writer.layout())?;
        let receipt = LinkReceipt {
            root: writer.identity().map_err(Failure::from)?,
            store: state.identity.clone(),
            row: row.clone(),
        };
        state.links.push(row);
        publish(writer, &file, Some(&previous), &state)?;

        Ok((record, receipt))
    }

    #[tracing::instrument(name = "store.finalise_link", level = "debug", skip_all)]
    fn finalise_link(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: LinkReceipt,
        kernel_id: NonZeroU32,
    ) -> Result<StoredLink, EffectFailure<LinkReceipt, Error>> {
        let result = (|| -> Result<_, Failure> {
            let file = writer.open_store_snapshot()?;
            let (previous, mut state) = read(&file)?;
            let index = check(writer, &state, &receipt)?;

            if receipt.row.state != LinkProgress::Pending {
                return Err(Failure::Invalid("link is not pending"));
            }

            if state
                .links
                .iter()
                .any(|link| link.state == (LinkProgress::Attached { kernel_id }))
            {
                return Err(Failure::Invalid("kernel link ID already recorded"));
            }

            state.links[index].state = LinkProgress::Attached { kernel_id };
            let record = state.links[index].record(writer.layout())?;
            publish(writer, &file, Some(&previous), &state)?;

            Ok(record)
        })();

        result.map_err(|cause| EffectFailure {
            remaining: receipt,
            cause: cause.into(),
        })
    }

    #[tracing::instrument(name = "store.observe_link", level = "debug", skip_all, err)]
    fn observe_link(
        &self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU64,
    ) -> Result<LinkObservation<Self>, Error> {
        let file = writer.open_store_snapshot().map_err(Failure::from)?;
        let (_, state) = read(&file)?;
        if state.xdp.iter().any(|row| row.link_id == id) {
            return Err(
                Failure::Unsupported("XDP links require dispatcher snapshot operations").into(),
            );
        }
        let Some(row) = state.links.iter().find(|row| row.id == id) else {
            return Ok(None);
        };
        let record = row.record(writer.layout())?;
        let receipt = LinkReceipt {
            root: writer.identity().map_err(Failure::from)?,
            store: state.identity.clone(),
            row: row.clone(),
        };

        Ok(Some((record, receipt)))
    }

    #[tracing::instrument(name = "store.delete_link", level = "debug", skip_all)]
    fn delete_link(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: LinkReceipt,
    ) -> Result<(), EffectFailure<LinkReceipt, Error>> {
        let result = (|| -> Result<(), Failure> {
            let file = writer.open_store_snapshot()?;
            let (previous, mut state) = read(&file)?;
            let index = check(writer, &state, &receipt)?;
            state.links.remove(index);
            publish(writer, &file, Some(&previous), &state)
        })();

        result.map_err(|cause| EffectFailure {
            remaining: receipt,
            cause: cause.into(),
        })
    }
}
