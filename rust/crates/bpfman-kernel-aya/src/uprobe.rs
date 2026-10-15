//! Native host user-space entry/return probes with confined pin adoption.
use crate::{AyaLink, AyaUprobe, Kernel, failure::filesystem};
use bpfman_fs::{
    KernelResult, PinSource, PinnedProgram, PreparedUprobeAttach, ProgramInspection, RuntimeWriter,
    UprobeKernel, UprobeProgram,
};
use bpfman_kernel::{Error, UprobeLinks};
use bpfman_model::UprobeLink;
use std::{
    num::NonZeroU32,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
};

impl UprobeLinks for Kernel {
    type PreparedUprobe = PreparedUprobeAttach<AyaUprobe>;

    fn prepare_uprobe(
        &self,
        writer: &RuntimeWriter<'_>,
        program: NonZeroU32,
        target: &UprobeLink,
    ) -> Result<Self::PreparedUprobe, Error> {
        writer
            .prepare_uprobe_attach(self, program, target.retprobe())
            .map_err(filesystem)
    }

    fn attach_uprobe(
        &self,
        writer: &RuntimeWriter<'_>,
        prepared: Self::PreparedUprobe,
        target: &UprobeLink,
    ) -> Result<Self::LiveTracepoint, Error> {
        prepared.attach(writer, target).map_err(filesystem)
    }
}

impl UprobeKernel for Kernel {
    type Uprobe = AyaUprobe;

    fn uprobe_at(
        &self,
        source: PinSource<'_>,
        retprobe: bool,
    ) -> KernelResult<(PinnedProgram, AyaUprobe)> {
        let info = self.program_at(source)?;
        let kind = if retprobe {
            aya::programs::ProbeKind::Return
        } else {
            aya::programs::ProbeKind::Entry
        };
        let program = aya::programs::UProbe::from_pin(source.path(), kind)?;
        Ok((info, AyaUprobe(program)))
    }
}

impl UprobeProgram for AyaUprobe {
    type Link = AyaLink;

    fn attach(&mut self, target: &UprobeLink) -> KernelResult<AyaLink> {
        let scope = target.pid().map_or(
            aya::programs::uprobe::UProbeScope::AllProcesses,
            aya::programs::uprobe::UProbeScope::OneProcess,
        );
        let point = match target.function() {
            Some(name) => {
                aya::programs::uprobe::UProbeAttachLocation::SymbolOffset(name, target.offset())
            }
            None => aya::programs::uprobe::UProbeAttachLocation::AbsoluteOffset(target.offset()),
        };

        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
            .open(target.target())?;

        if !file.metadata()?.is_file() {
            return Err(std::io::Error::other("uprobe target must be a regular file").into());
        }

        let path = format!("/proc/self/fd/{}", file.as_raw_fd());
        let id = self.0.attach(point, path, scope)?;
        let link: aya::programs::links::FdLink = self.0.take_link(id)?.try_into()?;
        Ok(AyaLink(link))
    }
}
