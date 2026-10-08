use crate::{
    Kernel, PreparedTc, TcDispatcher, TcExtension, TcFilter, TcStage, XdpNamespace,
    failure::{filesystem, map},
    tc_netlink,
};
use aya::programs::{
    Extension, SchedClassifier, TcAttachType,
    tc::{NlOptions, TcAttachOptions},
};
use bpfman_core::EffectFailure;
use bpfman_fs::{
    ExtensionProgram, KernelResult, PinSource, PinTarget, ProgramPinning, RuntimeWriter, TcKernel,
};
use bpfman_kernel::{Acquisition, Error, ErrorKind, Removal, TcLifecycle};
use bpfman_model::{InterfaceName, NetworkNamespace, TcProceedOn, TcSnapshot, XdpKey};
use std::{io, num::NonZeroU32};

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn kernel(e: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::new(ErrorKind::Unavailable, "TC ingress kernel operation", e)
}

fn before<T>(cause: Error) -> EffectFailure<Option<T>, Error> {
    EffectFailure {
        cause,
        remaining: None,
    }
}

impl ProgramPinning for TcDispatcher {
    fn id(&self) -> KernelResult<u32> {
        Ok(self
            .0
            .program("tc_dispatcher")
            .ok_or_else(|| invalid("TC dispatcher missing"))?
            .info()?
            .id())
    }

    fn pin(&mut self, target: PinTarget<'_>) -> KernelResult<()> {
        self.0
            .program_mut("tc_dispatcher")
            .ok_or_else(|| invalid("TC dispatcher missing"))?
            .pin(target.path())
            .map_err(Into::into)
    }
}

impl ExtensionProgram for TcExtension {
    type Dispatcher = TcDispatcher;
    type Link = crate::AyaLink;

    fn attach(
        &mut self,
        dispatcher: &TcDispatcher,
        slot: bpfman_model::XdpSlot,
    ) -> KernelResult<Self::Link> {
        if slot != bpfman_model::XdpSlot::FIRST {
            return Err(invalid("only TC slot zero is supported").into());
        }
        let target: &SchedClassifier = dispatcher
            .0
            .program("tc_dispatcher")
            .ok_or_else(|| invalid("TC dispatcher missing"))?
            .try_into()?;
        let id = self.0.attach_to_program(target.fd()?, "prog0")?;
        Ok(crate::AyaLink(self.0.take_link(id)?.into()))
    }
}

impl TcKernel for Kernel {
    type Extension = TcExtension;

    fn tc_extension_at(
        &self,
        source: PinSource<'_>,
    ) -> KernelResult<(bpfman_fs::PinnedProgram, TcExtension)> {
        use bpfman_fs::ProgramInspection;
        let info = self.program_at(source)?;
        // The filesystem validates EXT identity before any traffic effects.
        let extension = Extension::from_pin(source.path())?;
        Ok((info, TcExtension(extension)))
    }
}

impl TcLifecycle for Kernel {
    type Prepared = PreparedTc;
    type Stage = TcStage;
    type Filter = TcFilter;

    fn prepare_tc(
        &self,
        w: &RuntimeWriter<'_>,
        program: NonZeroU32,
        interface: &InterfaceName,
        netns: &NetworkNamespace,
    ) -> Result<(XdpKey, PreparedTc), Error> {
        let namespace = XdpNamespace::open(interface, netns).map_err(kernel)?;
        let key = namespace.key();
        let program = w.prepare_tc_program(self, program).map_err(filesystem)?;
        Ok((key, PreparedTc { program, namespace }))
    }

    fn stage_tc(
        &self,
        w: &RuntimeWriter<'_>,
        mut prepared: PreparedTc,
        actions: TcProceedOn,
    ) -> Acquisition<TcStage> {
        prepared
            .namespace
            .validate()
            .map_err(kernel)
            .map_err(before)?;
        let root = w.identity().map_err(filesystem).map_err(before)?;
        let config = bpfman_model::tc_config(actions);
        let mut bpf = aya::EbpfLoader::new()
            .override_global("CONFIG", config.as_slice(), true)
            .load(crate::verification::tc_dispatcher_bytes())
            .map_err(kernel)
            .map_err(before)?;
        let program: &mut SchedClassifier = bpf
            .program_mut("tc_dispatcher")
            .ok_or_else(|| before(kernel(invalid("TC dispatcher missing"))))?
            .try_into()
            .map_err(kernel)
            .map_err(before)?;
        program.load().map_err(kernel).map_err(before)?;
        let mut dispatcher = TcDispatcher(bpf);
        let result = prepared
            .program
            .stage(w, prepared.namespace.key(), &mut dispatcher)
            .map_err(map);
        match result {
            Ok(pins) => Ok(TcStage {
                pins,
                dispatcher: Some(dispatcher),
                namespace: prepared.namespace,
                root,
            }),
            Err(f) => Err(EffectFailure {
                cause: f.cause,
                remaining: f.remaining.map(|pins| TcStage {
                    pins,
                    dispatcher: Some(dispatcher),
                    namespace: prepared.namespace,
                    root,
                }),
            }),
        }
    }

    fn attach_tc_filter(&self, w: &RuntimeWriter<'_>, stage: &TcStage) -> Acquisition<TcFilter> {
        if w.identity().map_err(filesystem).map_err(before)? != stage.root {
            return Err(before(kernel(invalid(
                "TC stage belongs to another runtime",
            ))));
        }
        stage.namespace.validate().map_err(kernel).map_err(before)?;
        let mut outer = TcFilter {
            namespace: stage
                .namespace
                .duplicate()
                .map_err(kernel)
                .map_err(before)?,
            root: stage.root,
            dispatcher: stage
                .pins
                .ids()
                .map_err(filesystem)
                .map_err(before)?
                .0
                .get(),
            handle: None,
            clsact_owned: false,
        };
        let dispatcher = stage
            .dispatcher
            .as_ref()
            .ok_or_else(|| before(kernel(invalid("TC stage has no live dispatcher"))))?;
        // Retain a fresh owned program handle because attach_with_options needs &mut.
        let info = dispatcher
            .0
            .program("tc_dispatcher")
            .ok_or_else(|| before(kernel(invalid("TC dispatcher missing"))))?
            .info()
            .map_err(kernel)
            .map_err(before)?;
        let mut program = SchedClassifier::from_program_info(info, "tc_dispatcher".into())
            .map_err(kernel)
            .map_err(before)?;
        let result = stage.namespace.run_tc(|| {
            let ifindex = stage.namespace.key().ifindex.get();
            if crate::pin_syscall::interface(stage.namespace.tc_interface())? != ifindex {
                return Err(invalid("TC interface changed since admission"));
            }
            if !tc_netlink::clsact(ifindex)? {
                // EEXIST is a raced creator. Reinspect and reuse without taking ownership.
                match aya::programs::tc::qdisc_add_clsact(stage.namespace.tc_interface()) {
                    Ok(()) => outer.clsact_owned = true,
                    Err(e) => {
                        if !tc_netlink::clsact(ifindex)? {
                            return Err(io::Error::other(e));
                        }
                    }
                }
            }
            let id = program
                .attach_with_options(
                    stage.namespace.tc_interface(),
                    TcAttachType::Ingress,
                    TcAttachOptions::Netlink(NlOptions {
                        priority: 50,
                        ..Default::default()
                    }),
                )
                .map_err(io::Error::other)?;
            let link = program.take_link(id).map_err(io::Error::other)?;
            let handle = NonZeroU32::new(u32::from(link.handle().map_err(io::Error::other)?))
                .ok_or_else(|| invalid("TC filter returned zero handle"))?;
            outer.handle = Some(handle);
            // Netlink links carry only this tuple, no owned fd or allocation.
            // Transfer their detach obligation into our consuming, retryable receipt.
            std::mem::forget(link);
            Ok(())
        });
        let result = result.map_err(kernel).and_then(|()| {
            stage
                .pins
                .record_clsact(w, outer.clsact_owned)
                .map_err(filesystem)
        });
        match result {
            Ok(()) => Ok(outer),
            Err(cause) => Err(EffectFailure {
                cause,
                remaining: Some(outer),
            }),
        }
    }

    fn tc_ids(stage: &TcStage) -> Result<(NonZeroU32, NonZeroU32), Error> {
        stage.pins.ids().map_err(filesystem)
    }

    fn tc_filter(filter: &TcFilter) -> Result<(u16, NonZeroU32), Error> {
        Ok((
            50,
            filter
                .handle
                .ok_or_else(|| kernel(invalid("TC filter was not acquired")))?,
        ))
    }

    fn observe_tc(
        &self,
        w: &RuntimeWriter<'_>,
        snapshot: &TcSnapshot,
    ) -> Result<(TcStage, TcFilter), Error> {
        let d = &snapshot.details;
        if d.filter_priority != 50 {
            return Err(kernel(invalid("unsupported TC filter priority")));
        }
        let root = w.identity().map_err(filesystem)?;
        let namespace = XdpNamespace::open(&d.interface, &d.netns).map_err(kernel)?;
        if namespace.key() != d.key {
            return Err(kernel(invalid(
                "TC namespace/interface differs from snapshot",
            )));
        }
        let pins = w.observe_tc_pins(self, snapshot).map_err(filesystem)?;
        let clsact_owned = pins.clsact_owned(w).map_err(filesystem)?;
        namespace
            .run_tc(|| {
                tc_netlink::filter(
                    d.key.ifindex.get(),
                    d.filter_handle.get(),
                    d.dispatcher_id.get(),
                )
            })
            .map_err(kernel)?;
        let filter = TcFilter {
            namespace: namespace.duplicate().map_err(kernel)?,
            root,
            dispatcher: d.dispatcher_id.get(),
            handle: Some(d.filter_handle),
            clsact_owned,
        };
        Ok((
            TcStage {
                pins,
                dispatcher: None,
                namespace,
                root,
            },
            filter,
        ))
    }

    fn remove_tc_filter(&self, w: &RuntimeWriter<'_>, mut filter: TcFilter) -> Removal<TcFilter> {
        let result = (|| -> Result<(), Error> {
            if w.identity().map_err(filesystem)? != filter.root {
                return Err(kernel(invalid("TC filter belongs to another runtime")));
            }
            filter
                .namespace
                .run_tc(|| {
                    let ifindex = filter.namespace.key().ifindex.get();
                    if let Some(handle) = filter.handle {
                        tc_netlink::detach(ifindex, handle.get(), filter.dispatcher)?;
                        filter.handle = None;
                    }
                    if filter.clsact_owned {
                        tc_netlink::reclaim_clsact(ifindex)?;
                        filter.clsact_owned = false;
                    }
                    Ok(())
                })
                .map_err(kernel)
        })();
        result.map_err(|cause| EffectFailure {
            cause,
            remaining: filter,
        })
    }

    fn remove_tc_stage(&self, w: &RuntimeWriter<'_>, stage: TcStage) -> Removal<TcStage> {
        let TcStage {
            pins,
            dispatcher,
            namespace,
            root,
        } = stage;
        w.remove_tc_pins(pins).map_err(|f| EffectFailure {
            cause: filesystem(f.cause),
            remaining: TcStage {
                pins: f.remaining,
                dispatcher,
                namespace,
                root,
            },
        })
    }
}
