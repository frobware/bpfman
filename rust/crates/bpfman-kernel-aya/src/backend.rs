use crate::{Error, ErrorKind, Kernel, observe};
use bpfman_fs::{ObservedMapPin, RuntimeDirectory};
use bpfman_kernel::{LinkObservations, ProgramObservations};
use bpfman_model::{KernelLink, KernelMap, KernelProgram, ProgramStats, XdpLink};
use std::num::{NonZeroU32, NonZeroU64};

fn filesystem(error: bpfman_fs::Error) -> Error {
    let kind = match error.kind() {
        bpfman_fs::ErrorKind::UnsafeLayout => ErrorKind::InvalidData,
        _ => ErrorKind::Unavailable,
    };
    Error::new(kind, "observe managed kernel pin", error)
}

impl ProgramObservations for Kernel {
    fn program(&self, id: NonZeroU32) -> Result<(KernelProgram, Option<ProgramStats>), Error> {
        observe::observe_program(id)
    }

    fn map(&self, id: u32) -> Result<KernelMap, Error> {
        observe::observe_map(id)
    }

    fn map_pins(
        &self,
        runtime: &RuntimeDirectory,
        map_set: NonZeroU32,
    ) -> Result<Vec<ObservedMapPin>, Error> {
        runtime.read_map_pins(map_set).map_err(filesystem)
    }
}

impl LinkObservations for Kernel {
    fn tracepoint_link(&self, id: NonZeroU32) -> Result<KernelLink, Error> {
        observe::observe_tracepoint_link(id)
    }

    fn extension_link(&self, id: NonZeroU32) -> Result<KernelLink, Error> {
        observe::observe_extension_link(id)
    }

    fn tracepoint_pin(
        &self,
        runtime: &RuntimeDirectory,
        id: NonZeroU64,
    ) -> Result<Option<KernelLink>, Error> {
        runtime.read_link_pin(id).map_err(filesystem)
    }

    fn extension_pin(
        &self,
        runtime: &RuntimeDirectory,
        link: &XdpLink,
    ) -> Result<Option<KernelLink>, Error> {
        runtime.read_xdp_link_pin(link).map_err(filesystem)
    }
}
