//! Native classifier links, separate from legacy qdiscs and dispatchers.
use crate::{AyaLink, AyaTcx, Kernel, XdpNamespace, failure::filesystem};
use bpfman_fs::{
    KernelResult, PinSource, PinnedProgram, PreparedTcxAttach, ProgramInspection, RuntimeWriter,
    TcxKernel, TcxProgram,
};
use bpfman_kernel::{Error, ErrorKind, TcxLinks};
use bpfman_model::{
    InterfaceName, KernelLink, KernelLinkDetails, NetworkNamespace, TcDirection, TcKey, TcxLink,
    TcxOrder,
};
use std::{num::NonZeroU32, os::fd::AsFd};

fn invalid(message: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

fn failure(error: std::io::Error) -> Error {
    let kind = if error.raw_os_error() == Some(libc::ENOENT) {
        ErrorKind::Missing
    } else if error.kind() == std::io::ErrorKind::InvalidData {
        ErrorKind::InvalidData
    } else {
        ErrorKind::Unavailable
    };
    Error::new(kind, "TCX kernel operation", error)
}

impl TcxLinks for Kernel {
    type PreparedTcx = PreparedTcxAttach<AyaTcx>;

    fn prepare_tcx(
        &self,
        w: &RuntimeWriter<'_>,
        program: NonZeroU32,
        interface: &InterfaceName,
        netns: &NetworkNamespace,
        direction: TcDirection,
        priority: u32,
    ) -> Result<(TcxLink, Self::PreparedTcx), Error> {
        let ns = XdpNamespace::open(interface, netns).map_err(failure)?;
        let key = ns.key();
        let target = TcxLink {
            key: TcKey {
                nsid: key.nsid,
                ifindex: key.ifindex,
                direction,
            },
            interface: interface.clone(),
            netns: netns.clone(),
            priority,
            position: 0,
        };
        let prepared = w
            .prepare_tcx_attach(self, program, &target)
            .map_err(filesystem)?;
        Ok((target, prepared))
    }

    fn attach_tcx(
        &self,
        w: &RuntimeWriter<'_>,
        prepared: Self::PreparedTcx,
        order: TcxOrder,
    ) -> Result<Self::LiveTracepoint, Error> {
        prepared.attach(w, order).map_err(filesystem)
    }
}

impl TcxKernel for Kernel {
    type Program = AyaTcx;

    fn tcx_at(
        &self,
        source: PinSource<'_>,
        target: &TcxLink,
    ) -> KernelResult<(PinnedProgram, AyaTcx)> {
        let info = self.program_at(source)?;
        let program = aya::programs::SchedClassifier::from_pin(source.path())?;
        let namespace = XdpNamespace::open(&target.interface, &target.netns)?;
        if namespace.key().nsid != target.key.nsid || namespace.key().ifindex != target.key.ifindex
        {
            return Err(invalid("TCX namespace/interface changed during admission").into());
        }
        Ok((info, AyaTcx { program, namespace }))
    }
}

impl TcxProgram for AyaTcx {
    type Link = AyaLink;

    fn attach(&mut self, target: &TcxLink, order: TcxOrder) -> KernelResult<AyaLink> {
        self.namespace.validate()?;
        let (order, _anchor) = crate::pin_syscall::tcx_order(order)?;
        let direction = match target.key.direction {
            TcDirection::Ingress => aya::programs::TcAttachType::Ingress,
            TcDirection::Egress => aya::programs::TcAttachType::Egress,
        };
        self.namespace
            .run_tc(|| {
                if crate::pin_syscall::interface(target.interface.as_str())?
                    != target.key.ifindex.get()
                {
                    return Err(invalid("TCX interface changed since admission"));
                }
                let id = self
                    .program
                    .attach_with_options(
                        target.interface.as_str(),
                        direction,
                        aya::programs::tc::TcAttachOptions::TcxOrder(order),
                    )
                    .map_err(std::io::Error::other)?;
                let link: aya::programs::links::FdLink = self
                    .program
                    .take_link(id)
                    .map_err(std::io::Error::other)?
                    .try_into()
                    .map_err(std::io::Error::other)?;
                Ok(AyaLink(link))
            })
            .map_err(Into::into)
    }
}

pub(super) fn observe(id: NonZeroU32) -> Result<KernelLink, Error> {
    // Obtain an owned descriptor before querying identity, avoiding an ID reuse race.
    let fd = crate::pin_syscall::by_id(id.get()).map_err(failure)?;
    let info = crate::pin_syscall::info(fd.as_fd()).map_err(failure)?;
    if info.kind != 11 {
        return Err(failure(invalid("expected TCX kernel link")));
    }
    let direction = match info.data[1] {
        46 => TcDirection::Ingress,
        47 => TcDirection::Egress,
        _ => return Err(failure(invalid("unexpected TCX attach type"))),
    };
    Ok(KernelLink {
        id,
        program_id: NonZeroU32::new(info.program)
            .ok_or_else(|| failure(invalid("zero TCX program ID")))?,
        details: KernelLinkDetails::Tcx {
            ifindex: info.data[0],
            direction,
        },
    })
}
