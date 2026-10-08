use crate::{
    Kernel, PreparedTc, TcDispatcher, TcExtension, TcFilter, TcStage, TcSwitch, XdpNamespace,
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
        let target: &SchedClassifier = dispatcher
            .0
            .program("tc_dispatcher")
            .ok_or_else(|| invalid("TC dispatcher missing"))?
            .try_into()?;
        let id = self
            .0
            .attach_to_program(target.fd()?, &format!("prog{}", slot.index()))?;
        Ok(crate::AyaLink(self.0.take_link(id)?.into()))
    }
}

impl TcKernel for Kernel {
    type Extension = TcExtension;
    type Target = SchedClassifier;

    fn tc_target_at(
        &self,
        source: PinSource<'_>,
    ) -> KernelResult<(bpfman_fs::PinnedProgram, SchedClassifier)> {
        use bpfman_fs::ProgramInspection;
        Ok((
            self.program_at(source)?,
            SchedClassifier::from_pin(source.path())?,
        ))
    }

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
    type Switch = TcSwitch;

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
        prepared: PreparedTc,
        actions: TcProceedOn,
    ) -> Acquisition<TcStage> {
        self.stage_tc_revision(w, vec![prepared], NonZeroU32::MIN, &[actions], false)
    }

    fn stage_tc_revision(
        &self,
        w: &RuntimeWriter<'_>,
        prepared: Vec<PreparedTc>,
        revision: NonZeroU32,
        actions: &[TcProceedOn],
        clsact_owned: bool,
    ) -> Acquisition<TcStage> {
        let Some(first) = prepared.first() else {
            return Err(before(kernel(invalid("empty TC revision"))));
        };
        let key = first.namespace.key();
        if prepared.len() != actions.len() {
            return Err(before(kernel(invalid("TC member count mismatch"))));
        }
        for p in &prepared {
            p.namespace.validate().map_err(kernel).map_err(before)?;
            if p.namespace.key() != key {
                return Err(before(kernel(invalid("TC interface changed"))));
            }
        }
        let namespace = first
            .namespace
            .duplicate()
            .map_err(kernel)
            .map_err(before)?;
        let root = w.identity().map_err(filesystem).map_err(before)?;
        let config = bpfman_model::tc_revision_config(actions)
            .map_err(kernel)
            .map_err(before)?;
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
        let mut programs: Vec<_> = prepared.into_iter().map(|p| p.program).collect();
        match bpfman_fs::TcProgram::stage_revision(
            &mut programs,
            w,
            key,
            revision,
            &mut dispatcher,
            clsact_owned,
        )
        .map_err(map)
        {
            Ok(pins) => Ok(TcStage {
                pins,
                dispatcher: Some(dispatcher),
                namespace,
                root,
            }),
            Err(f) => Err(EffectFailure {
                cause: f.cause,
                remaining: f.remaining.map(|pins| TcStage {
                    pins,
                    dispatcher: Some(dispatcher),
                    namespace,
                    root,
                }),
            }),
        }
    }

    fn tc_revision_ids(stage: &TcStage) -> Result<(NonZeroU32, Vec<NonZeroU32>), Error> {
        stage
            .pins
            .revision_ids()
            .map(|(id, links)| (id, links.to_vec()))
            .map_err(filesystem)
    }

    fn tc_clsact_owned(&self, w: &RuntimeWriter<'_>, stage: &TcStage) -> Result<bool, Error> {
        stage.pins.clsact_owned(w).map_err(filesystem)
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
        let full =
            bpfman_model::TcDispatcherSnapshot::new(vec![snapshot.clone()]).map_err(kernel)?;
        self.observe_tc_dispatcher(w, &full)
    }

    fn observe_tc_dispatcher(
        &self,
        w: &RuntimeWriter<'_>,
        snapshot: &bpfman_model::TcDispatcherSnapshot,
    ) -> Result<(TcStage, TcFilter), Error> {
        let d = &snapshot
            .members()
            .first()
            .ok_or_else(|| kernel(invalid("empty TC snapshot")))?
            .details;

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
        let pins = w
            .observe_tc_dispatcher_pins(self, snapshot)
            .map_err(filesystem)?;
        let clsact_owned = pins.clsact_owned(w).map_err(filesystem)?;
        let present = namespace
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
            handle: present.then_some(d.filter_handle),
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

    fn switch_tc(
        &self,
        w: &RuntimeWriter<'_>,
        filter: &TcFilter,
        old: &TcStage,
        new: &TcStage,
    ) -> Acquisition<TcSwitch> {
        let root = w.identity().map_err(filesystem).map_err(before)?;
        if root != filter.root
            || root != old.root
            || root != new.root
            || old.namespace.key() != new.namespace.key()
            || old.namespace.key() != filter.namespace.key()
        {
            return Err(before(kernel(invalid("TC switch authority differs"))));
        }
        old.namespace.validate().map_err(kernel).map_err(before)?;
        new.namespace.validate().map_err(kernel).map_err(before)?;
        let (old_id, _) = old
            .pins
            .revision_ids()
            .map_err(filesystem)
            .map_err(before)?;
        let (new_id, _) = new
            .pins
            .revision_ids()
            .map_err(filesystem)
            .map_err(before)?;
        if old_id.get() != filter.dispatcher {
            return Err(before(kernel(invalid("TC switch target mismatch"))));
        }
        let handle = filter
            .handle
            .ok_or_else(|| before(kernel(invalid("TC filter missing"))))?;
        let mut receipt = TcSwitch {
            namespace: filter
                .namespace
                .duplicate()
                .map_err(kernel)
                .map_err(before)?,
            root,
            handle,
            old_id: old_id.get(),
            new_id: new_id.get(),
            old: old
                .pins
                .target(w, self)
                .map_err(filesystem)
                .map_err(before)?,
            new: new
                .pins
                .target(w, self)
                .map_err(filesystem)
                .map_err(before)?,
        };
        // Route netlink has no expected-program CAS. Recheck the exact tuple just
        // before Aya replaces it; concurrent privileged writers remain outside this lock.
        receipt
            .namespace
            .run_tc(|| {
                if !tc_netlink::filter(
                    receipt.namespace.key().ifindex.get(),
                    handle.get(),
                    receipt.old_id,
                )? {
                    return Err(invalid("TC filter disappeared before replacement"));
                }
                Ok(())
            })
            .map_err(kernel)
            .map_err(before)?;
        let result = receipt
            .namespace
            .run_tc(|| replace_filter(&receipt.namespace, handle, &mut receipt.new));
        match result {
            Ok(()) => Ok(receipt),
            Err(cause) => Err(EffectFailure {
                cause: kernel(cause),
                remaining: Some(receipt),
            }),
        }
    }

    fn restore_tc(&self, w: &RuntimeWriter<'_>, mut receipt: TcSwitch) -> Removal<TcSwitch> {
        let result = (|| -> Result<(), Error> {
            if w.identity().map_err(filesystem)? != receipt.root {
                return Err(kernel(invalid("TC restoration belongs to another runtime")));
            }
            receipt.namespace.validate().map_err(kernel)?;
            receipt
                .namespace
                .run_tc(|| {
                    // An uncertain failed switch can already be on the original target.
                    match tc_netlink::filter(
                        receipt.namespace.key().ifindex.get(),
                        receipt.handle.get(),
                        receipt.old_id,
                    ) {
                        Ok(true) => return Ok(()),
                        Ok(false) => {
                            return Err(invalid("TC filter disappeared before restoration"));
                        }
                        Err(_) => {}
                    }
                    if !tc_netlink::filter(
                        receipt.namespace.key().ifindex.get(),
                        receipt.handle.get(),
                        receipt.new_id,
                    )? {
                        return Err(invalid("TC replacement filter missing"));
                    }
                    replace_filter(&receipt.namespace, receipt.handle, &mut receipt.old)
                })
                .map_err(kernel)
        })();
        result.map_err(|cause| EffectFailure {
            cause,
            remaining: receipt,
        })
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

fn replace_filter(
    namespace: &XdpNamespace,
    handle: NonZeroU32,
    program: &mut SchedClassifier,
) -> io::Result<()> {
    use aya::programs::tc::{SchedClassifierLink, TcHandle};
    if crate::pin_syscall::interface(namespace.tc_interface())? != namespace.key().ifindex.get() {
        return Err(invalid("TC interface changed before switch"));
    }
    let link = SchedClassifierLink::attached(
        namespace.tc_interface(),
        TcAttachType::Ingress,
        50,
        TcHandle::from(handle.get()),
        None,
    )?;
    let id = program.attach_to_link(link).map_err(io::Error::other)?;
    let link = program.take_link(id).map_err(io::Error::other)?;
    // Descriptor-free netlink ownership transfers to the retained switch/filter receipt.
    std::mem::forget(link);
    Ok(())
}
