use super::{Artifacts, UnloadEffects};
use crate::{UnloadCause, unload_error::Cause};
use bpfman_core::EffectFailure;
use bpfman_fs::{Bytecode, RuntimeWriter};
use bpfman_model::LinkState;
use bpfman_store::{LinkReader, LinkStore, OpenStore, UnloadStore};
use std::num::NonZeroU32;

pub(crate) struct Effects<'a, S, K>(pub(crate) &'a S, pub(crate) &'a K);

fn map_failure<R, E: Into<UnloadCause>>(
    failure: EffectFailure<R, E>,
) -> EffectFailure<R, UnloadCause> {
    EffectFailure {
        remaining: failure.remaining,
        cause: failure.cause.into(),
    }
}

impl<
    S: OpenStore + UnloadStore + LinkStore,
    K: bpfman_kernel::ProgramResources + bpfman_kernel::TracepointLinks,
> UnloadEffects for Effects<'_, S, K>
where
    S::Reader: LinkReader,
{
    type LinkPin = K::LinkPin;
    type LinkRecord = S::LinkReceipt;
    type Pin = K::ProgramPin;
    type Record = S::ProgramReceipt;
    type Map = K::MapPin;
    type Directory = K::MapDirectory;
    type MapSet = S::MapSetReceipt;
    type Bytecode = Bytecode;
    type Error = UnloadCause;

    fn cancelled(&self) -> UnloadCause {
        Cause::Cancelled.into()
    }

    fn observe_store(
        &mut self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<(S::ProgramReceipt, S::MapSetReceipt), UnloadCause> {
        self.0
            .observe_unload(writer, id)?
            .ok_or_else(|| Cause::NotFound(id).into())
    }

    fn observe_artifacts(
        &mut self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<Artifacts<K::ProgramPin, K::MapPin, K::MapDirectory, Bytecode>, UnloadCause> {
        let found = self.1.observe_unload(writer, id)?;

        Ok(Artifacts {
            pin: found.program,
            maps: found.maps,
            directory: found.directory,
            bytecode: found.bytecode,
        })
    }

    fn observe_links(
        &mut self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<Vec<bpfman_core::UnloadLink<Self::LinkPin, Self::LinkRecord>>, UnloadCause> {
        let mut records = self.0.open(writer)?.read_links()?;
        records.retain(|record| record.program_id == id);
        records.sort_by_key(|record| record.id);
        let mut links = Vec::new();

        for record in records {
            if matches!(
                record.details,
                bpfman_model::LinkDetails::Xdp(_) | bpfman_model::LinkDetails::Tc(_)
            ) {
                // The enclosing unload prerequisite stage handles dispatcher links.
                continue;
            }
            let (current, receipt) = self
                .0
                .observe_link(writer, record.id)?
                .ok_or(Cause::Invalid("link disappeared during unload preflight"))?;

            if current != record {
                return Err(Cause::Invalid("link changed during unload preflight").into());
            }
            if writer.layout().link_pin_path(record.id).to_str() != Some(record.pin_path.as_str()) {
                return Err(
                    Cause::Invalid("link pin differs from canonical runtime layout").into(),
                );
            }

            let kernel = match record.state {
                // Pending intent has no recorded kernel ID. The filesystem
                // adapter still validates the pin's program, type and identity
                // before issuing an owned receipt for conditional removal.
                LinkState::Pending => None,
                LinkState::Attached { kernel_id } => Some(kernel_id),
            };
            let pin = self
                .1
                .observe_link_pin(writer, record.id, record.program_id, kernel)?;
            links.push(bpfman_core::UnloadLink {
                id: record.id,
                pin,
                record: receipt,
            });
        }

        Ok(links)
    }

    fn unpin_link(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::LinkPin,
    ) -> Result<(), EffectFailure<Self::LinkPin, UnloadCause>> {
        self.1.remove_link(writer, receipt).map_err(map_failure)
    }

    fn delete_link(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::LinkRecord,
    ) -> Result<(), EffectFailure<Self::LinkRecord, UnloadCause>> {
        self.0.delete_link(writer, receipt).map_err(map_failure)
    }

    fn unpin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: K::ProgramPin,
    ) -> Result<(), EffectFailure<K::ProgramPin, UnloadCause>> {
        self.1.remove_program(writer, receipt).map_err(map_failure)
    }

    fn delete_record(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: S::ProgramReceipt,
    ) -> Result<(), EffectFailure<S::ProgramReceipt, UnloadCause>> {
        self.0.delete_program(writer, receipt).map_err(map_failure)
    }

    fn remove_map(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: K::MapPin,
    ) -> Result<(), EffectFailure<K::MapPin, UnloadCause>> {
        self.1.remove_map(writer, receipt).map_err(map_failure)
    }

    fn remove_directory(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: K::MapDirectory,
    ) -> Result<(), EffectFailure<K::MapDirectory, UnloadCause>> {
        self.1
            .remove_map_directory(writer, receipt)
            .map_err(map_failure)
    }

    fn delete_map_set(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: S::MapSetReceipt,
    ) -> Result<(), EffectFailure<S::MapSetReceipt, UnloadCause>> {
        self.0.delete_map_set(writer, receipt).map_err(map_failure)
    }

    fn remove_bytecode(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Bytecode,
    ) -> Result<(), EffectFailure<Bytecode, UnloadCause>> {
        writer.remove_bytecode(receipt).map_err(map_failure)
    }
}
