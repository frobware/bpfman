use super::*;
use bpfman_model::{InterfaceName, NetworkNamespace, Tracepoint, TracingKind};
use bpfman_runtime::{
    KprobeAttach, TcAttach, TcxAttach, TracepointAttach, TracingAttach, UprobeAttach, XdpAttach,
};

impl<S, K, I> Service<S, K, I>
where
    S: bpfman_store::UnloadStore
        + bpfman_store::CommitLoad
        + bpfman_store::LinkStore
        + bpfman_store::XdpReplacementStore
        + bpfman_store::TcStore
        + 'static,
    S::Reader: bpfman_store::LinkReader,
    K: Kernel + 'static,
    I: Images,
{
    pub(super) fn attach(
        &self,
        request: pb::AttachRequest,
        cancellation: &Cancellation,
    ) -> Result<pb::AttachResponse, Status> {
        use pb::attach_info::Info;
        let attach = request
            .attach
            .ok_or_else(|| Status::invalid_argument("attach info is required"))?;
        let info = attach
            .info
            .ok_or_else(|| Status::unimplemented("attach type not implemented"))?;
        let id = NonZeroU32::new(request.id)
            .ok_or_else(|| Status::invalid_argument("program ID must be nonzero"))?;
        // The Go service currently ignores attach metadata, position and the
        // kprobe container selector. Preserve that existing wire behavior.
        let record = match info {
            Info::XdpAttachInfo(info) => {
                let interface = info.iface.parse::<InterfaceName>().map_err(invalid)?;
                let mode = (self.mode)(&interface)?;
                let priority = u32::try_from(info.priority).map_err(invalid)?;
                let netns = namespace(info.netns)?;
                let proceed_on = if info.proceed_on.is_empty() {
                    Default::default()
                } else {
                    let mut mask = 0u32;
                    for code in info.proceed_on {
                        if !matches!(code, 0..=4 | 31) {
                            return Err(Status::invalid_argument("invalid XDP proceed_on"));
                        }
                        mask |= 1 << code;
                    }
                    bpfman_model::XdpProceedOn::try_from(mask).map_err(invalid)?
                };
                self.app
                    .attach_xdp_with_cancellation(
                        XdpAttach {
                            netns,
                            mode,
                            program_id: id,
                            interface,
                            priority,
                            proceed_on,
                            metadata: Default::default(),
                        },
                        cancellation,
                    )
                    .map_err(|e| {
                        let status = convert::attachment(e.primary_failure(), e.kind(), &e);
                        if e.unresolved() > 0 {
                            self.retain(Recovery::Xdp(e));
                        }
                        status
                    })?
            }
            Info::TcAttachInfo(info) => {
                let interface = info.iface.parse::<InterfaceName>().map_err(invalid)?;
                let direction = info.direction.parse().map_err(invalid)?;
                let priority = u32::try_from(info.priority).map_err(invalid)?;
                let netns = namespace(info.netns)?;
                let proceed_on = if info.proceed_on.is_empty() {
                    Default::default()
                } else {
                    let mut mask = 0u32;
                    for code in info.proceed_on {
                        if !matches!(code, -1..=8 | 30) {
                            return Err(Status::invalid_argument("invalid TC proceed_on"));
                        }
                        mask |= 1 << (code + 1);
                    }
                    bpfman_model::TcProceedOn::try_from(mask).map_err(invalid)?
                };
                self.app
                    .attach_tc_with_cancellation(
                        TcAttach {
                            netns,
                            program_id: id,
                            interface,
                            direction,
                            priority,
                            proceed_on,
                            metadata: Default::default(),
                        },
                        cancellation,
                    )
                    .map_err(|e| {
                        let status = convert::attachment(e.primary_failure(), e.kind(), &e);
                        if e.unresolved() > 0 {
                            self.retain(Recovery::Tc(e));
                        }
                        status
                    })?
            }
            Info::TcxAttachInfo(info) => {
                let interface = info.iface.parse::<InterfaceName>().map_err(invalid)?;
                let direction = info.direction.parse().map_err(invalid)?;
                let priority = u32::try_from(info.priority).map_err(invalid)?;
                let netns = namespace(info.netns)?;
                self.app
                    .attach_tcx_with_cancellation(
                        TcxAttach {
                            netns,
                            program_id: id,
                            interface,
                            direction,
                            priority,
                            metadata: Default::default(),
                        },
                        cancellation,
                    )
                    .map_err(|e| self.link_error(e))?
            }
            Info::TracepointAttachInfo(info) => {
                let target = info.tracepoint.parse::<Tracepoint>().map_err(invalid)?;
                self.app
                    .attach_tracepoint_with_cancellation(
                        TracepointAttach {
                            program_id: id,
                            target,
                            metadata: Default::default(),
                        },
                        cancellation,
                    )
                    .map_err(|e| self.link_error(e))?
            }
            Info::KprobeAttachInfo(info) => {
                let function = info.fn_name.parse().map_err(invalid)?;
                self.app
                    .attach_kprobe_with_cancellation(
                        KprobeAttach {
                            program_id: id,
                            function,
                            offset: info.offset,
                            metadata: Default::default(),
                        },
                        cancellation,
                    )
                    .map_err(|e| self.link_error(e))?
            }
            Info::UprobeAttachInfo(info) => {
                let pid = u32::try_from(info.pid.unwrap_or(0)).map_err(invalid)?;
                let container = u32::try_from(info.container_pid.unwrap_or(0)).map_err(invalid)?;
                let function = info
                    .fn_name
                    .filter(|s| !s.is_empty())
                    .map(|s| bpfman_model::Symbol::try_from(s.as_str()).map_err(invalid))
                    .transpose()?;
                let target =
                    bpfman_model::UprobeTarget::new(&info.target, function, info.offset, pid)
                        .and_then(|t| t.with_container_pid(container))
                        .map_err(invalid)?;
                self.app
                    .attach_uprobe_with_cancellation(
                        UprobeAttach {
                            program_id: id,
                            target,
                            metadata: Default::default(),
                        },
                        cancellation,
                    )
                    .map_err(|e| self.link_error(e))?
            }
            Info::FentryAttachInfo(_) => self.tracing(id, TracingKind::Fentry, cancellation)?,
            Info::FexitAttachInfo(_) => self.tracing(id, TracingKind::Fexit, cancellation)?,
            Info::LsmAttachInfo(_) => self.tracing(id, TracingKind::Lsm, cancellation)?,
        };
        Ok(pb::AttachResponse {
            link_id: record.id.get() as u32,
        })
    }

    fn tracing(
        &self,
        id: NonZeroU32,
        kind: TracingKind,
        cancellation: &Cancellation,
    ) -> Result<bpfman_model::StoredLink, Status> {
        self.app
            .attach_tracing_with_cancellation(
                TracingAttach {
                    program_id: id,
                    kind,
                    metadata: Default::default(),
                },
                cancellation,
            )
            .map_err(|e| self.link_error(e))
    }

    fn link_error(&self, error: bpfman_runtime::LinkError<S, K>) -> Status {
        let status = convert::attachment(error.primary_failure(), error.kind(), &error);
        if error.report().is_some_and(|r| r.unresolved() > 0) {
            self.retain(Recovery::Link(error));
        }
        status
    }
}

fn namespace(value: Option<String>) -> Result<NetworkNamespace, Status> {
    value
        .as_deref()
        .unwrap_or_default()
        .parse()
        .map_err(invalid)
}
