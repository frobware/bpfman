//! Native kernel entry/return probes with confined pin adoption.
use crate::{AyaKprobe, AyaLink, Kernel, failure::filesystem};
use bpfman_fs::{
    KernelResult, KprobeKernel, KprobeProgram, PinSource, PinnedProgram, PreparedKprobeAttach,
    ProgramInspection, RuntimeWriter,
};
use bpfman_kernel::{Error, KprobeLinks};
use bpfman_model::KprobeLink;
use std::num::NonZeroU32;

impl KprobeLinks for Kernel {
    type PreparedKprobe = PreparedKprobeAttach<AyaKprobe>;

    fn prepare_kprobe(
        &self,
        writer: &RuntimeWriter<'_>,
        program: NonZeroU32,
        target: &KprobeLink,
    ) -> Result<Self::PreparedKprobe, Error> {
        writer
            .prepare_kprobe_attach(self, program, target.retprobe)
            .map_err(filesystem)
    }

    fn attach_kprobe(
        &self,
        writer: &RuntimeWriter<'_>,
        prepared: Self::PreparedKprobe,
        target: &KprobeLink,
    ) -> Result<Self::LiveTracepoint, Error> {
        prepared.attach(writer, target).map_err(filesystem)
    }
}

impl KprobeKernel for Kernel {
    type Kprobe = AyaKprobe;

    fn kprobe_at(
        &self,
        source: PinSource<'_>,
        retprobe: bool,
    ) -> KernelResult<(PinnedProgram, AyaKprobe)> {
        let info = self.program_at(source)?;
        let kind = if retprobe {
            aya::programs::ProbeKind::Return
        } else {
            aya::programs::ProbeKind::Entry
        };
        let program = aya::programs::KProbe::from_pin(source.path(), kind)?;
        Ok((info, AyaKprobe(program)))
    }
}

impl KprobeProgram for AyaKprobe {
    type Link = AyaLink;

    fn attach(&mut self, target: &KprobeLink) -> KernelResult<AyaLink> {
        let id = self.0.attach(target.function.as_str(), target.offset)?;
        let link: aya::programs::links::FdLink = self.0.take_link(id)?.try_into()?;
        Ok(AyaLink::from_aya(link))
    }
}
