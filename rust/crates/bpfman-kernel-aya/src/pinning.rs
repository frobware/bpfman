//! Aya objects implement the filesystem's confined pin bridge.
use crate::{
    AyaExtension, AyaLink, AyaOuter, AyaTracepoint, Dispatcher, Kernel, LoadedObject, pin_syscall,
};
use aya::programs::{Extension, ProgramInfo, ProgramType, TracePoint, Xdp, links::FdLink};
use bpfman_fs::*;
use bpfman_model::{InterfaceName, KernelLink, KernelLinkDetails, Tracepoint, XdpKey};
use std::{num::NonZeroU32, os::fd::AsFd};

fn invalid(message: &'static str) -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message).into()
}

fn nz(id: u32) -> KernelResult<NonZeroU32> {
    NonZeroU32::new(id).ok_or_else(|| invalid("zero kernel identity"))
}

fn program(info: &ProgramInfo) -> KernelResult<PinnedProgram> {
    let kind = match info.program_type() {
        k if k == ProgramType::TracePoint.into() => PinProgramKind::Tracepoint,
        k if k == ProgramType::Extension.into() => PinProgramKind::Extension,
        k if k == ProgramType::Xdp.into() => PinProgramKind::Xdp,
        _ => PinProgramKind::Other,
    };
    Ok(PinnedProgram {
        id: info.id(),
        kind,
        map_ids: info.map_ids()?,
    })
}

impl ProgramInspection for Kernel {
    fn program_at(&self, source: PinSource<'_>) -> KernelResult<PinnedProgram> {
        program(&ProgramInfo::from_pin(source.path())?)
    }

    fn map_at(&self, source: PinSource<'_>) -> KernelResult<u32> {
        Ok(aya::maps::MapInfo::from_pin(source.path())?.id())
    }
}

impl ProgramPinning for LoadedObject {
    fn id(&self) -> KernelResult<u32> {
        Ok(self
            .bpf
            .program(&self.name)
            .ok_or_else(|| invalid("loaded program missing"))?
            .info()?
            .id())
    }

    fn pin(&mut self, target: PinTarget<'_>) -> KernelResult<()> {
        self.bpf
            .program_mut(&self.name)
            .ok_or_else(|| invalid("loaded program missing"))?
            .pin(target.path())
            .map_err(Into::into)
    }
}

pub(crate) struct MapRef<'a>(pub(crate) &'a aya::maps::Map);
impl MapPinning for MapRef<'_> {
    fn pin(&self, target: PinTarget<'_>) -> KernelResult<()> {
        self.0.pin(target.path()).map_err(Into::into)
    }
}

impl ProgramPinning for Dispatcher {
    fn id(&self) -> KernelResult<u32> {
        Ok(self
            .0
            .program("xdp_dispatcher")
            .ok_or_else(|| invalid("dispatcher missing"))?
            .info()?
            .id())
    }

    fn pin(&mut self, target: PinTarget<'_>) -> KernelResult<()> {
        let program: &mut Xdp = self
            .0
            .program_mut("xdp_dispatcher")
            .ok_or_else(|| invalid("dispatcher missing"))?
            .try_into()?;
        program.pin(target.path()).map_err(Into::into)
    }
}

impl LinkPinning for AyaLink {
    fn id(&self) -> KernelResult<u32> {
        Ok(self.0.info()?.id())
    }

    fn pin(self, target: PinTarget<'_>) -> KernelResult<()> {
        drop(self.0.pin(target.path())?);
        Ok(())
    }
}

impl LinkInspection for Kernel {
    fn link_at(&self, source: PinSource<'_>) -> KernelResult<KernelLink> {
        let fd = pin_syscall::open(source.path())?;
        let info = pin_syscall::info(fd.as_fd())?;
        let details = match info.kind {
            7 => KernelLinkDetails::PerfEvent,
            2 => KernelLinkDetails::Tracing {
                attach_type: info.data[0],
                target_obj_id: info.data[1],
                target_btf_id: info.data[2],
            },
            _ => return Err(invalid("unexpected pinned link type")),
        };
        Ok(KernelLink {
            id: nz(info.id)?,
            program_id: nz(info.program)?,
            details,
        })
    }
}

impl TracepointKernel for Kernel {
    type Tracepoint = AyaTracepoint;

    fn tracepoint_at(&self, source: PinSource<'_>) -> KernelResult<(PinnedProgram, AyaTracepoint)> {
        let info = ProgramInfo::from_pin(source.path())?;
        let observation = program(&info)?;
        let handle = TracePoint::from_program_info(info, "managed_tracepoint".into())?;
        Ok((observation, AyaTracepoint(handle)))
    }
}

impl TracepointProgram for AyaTracepoint {
    type Link = AyaLink;

    fn attach(&mut self, target: &Tracepoint) -> KernelResult<AyaLink> {
        let id = self.0.attach(target.group(), target.name())?;
        let link: FdLink = self.0.take_link(id)?.try_into()?;
        Ok(AyaLink(link))
    }
}

impl ExtensionProgram for AyaExtension {
    type Dispatcher = Dispatcher;
    type Link = AyaLink;

    fn attach(
        &mut self,
        dispatcher: &Dispatcher,
        slot: bpfman_model::XdpSlot,
    ) -> KernelResult<AyaLink> {
        let program: &Xdp = dispatcher
            .0
            .program("xdp_dispatcher")
            .ok_or_else(|| invalid("dispatcher missing"))?
            .try_into()?;
        let id = self
            .0
            .attach_to_program(program.fd()?, &format!("prog{}", slot.index()))?;
        Ok(AyaLink(self.0.take_link(id)?.into()))
    }
}

impl bpfman_fs::XdpSwitchKernel for Kernel {
    type Target = crate::AyaXdpTarget;

    fn target_at(&self, source: PinSource<'_>) -> KernelResult<(PinnedProgram, Self::Target)> {
        let info = ProgramInfo::from_pin(source.path())?;
        let observation = program(&info)?;
        Ok((observation, crate::AyaXdpTarget(info.fd()?)))
    }

    fn replace_outer(
        &self,
        outer: &AyaOuter,
        expected: &Self::Target,
        new: &Self::Target,
    ) -> KernelResult<()> {
        pin_syscall::replace(outer.0.as_fd(), expected.0.as_fd(), new.0.as_fd()).map_err(Into::into)
    }
}

impl OuterLink for AyaOuter {
    fn info(&self) -> KernelResult<OuterInfo> {
        let info = pin_syscall::info(self.0.as_fd())?;
        if info.kind != 6 {
            return Err(invalid("expected an outer XDP link"));
        }
        Ok(OuterInfo {
            id: info.id,
            program: info.program,
            ifindex: info.data[0],
        })
    }

    fn detach(&self) -> KernelResult<()> {
        pin_syscall::detach(self.0.as_fd()).map_err(Into::into)
    }

    fn pin(&self, target: PinTarget<'_>) -> KernelResult<()> {
        pin_syscall::pin(self.0.as_fd(), target.path()).map_err(Into::into)
    }
}

impl XdpKernel for Kernel {
    type Extension = AyaExtension;
    type Outer = AyaOuter;

    type Namespace = crate::XdpNamespace;

    fn interface(
        &self,
        name: &InterfaceName,
        netns: &bpfman_model::NetworkNamespace,
    ) -> KernelResult<(XdpKey, Self::Namespace)> {
        let namespace = crate::XdpNamespace::open(name, netns)?;
        Ok((namespace.key(), namespace))
    }

    fn validate_namespace(&self, namespace: &Self::Namespace) -> KernelResult<()> {
        namespace.validate().map_err(Into::into)
    }

    fn extension_at(&self, source: PinSource<'_>) -> KernelResult<(PinnedProgram, AyaExtension)> {
        let info = program(&ProgramInfo::from_pin(source.path())?)?;
        Ok((info, AyaExtension(Extension::from_pin(source.path())?)))
    }

    fn outer_at(&self, source: PinSource<'_>) -> KernelResult<AyaOuter> {
        Ok(AyaOuter(pin_syscall::open(source.path())?))
    }

    fn outer_by_id(&self, id: NonZeroU32) -> KernelResult<Option<AyaOuter>> {
        match pin_syscall::by_id(id.get()) {
            Ok(fd) => Ok(Some(AyaOuter(fd))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn attach_outer(
        &self,
        dispatcher: &Dispatcher,
        key: XdpKey,
        namespace: &Self::Namespace,
        mode: bpfman_model::XdpMode,
    ) -> KernelResult<AyaOuter> {
        let program: &Xdp = dispatcher
            .0
            .program("xdp_dispatcher")
            .ok_or_else(|| invalid("dispatcher missing"))?
            .try_into()?;
        Ok(AyaOuter(namespace.attach(
            program.fd()?.as_fd(),
            key,
            mode,
        )?))
    }
}
