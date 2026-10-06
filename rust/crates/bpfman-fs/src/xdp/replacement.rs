use super::*;
use crate::XdpSwitchKernel;

/// Retained live link and both targets. This grants restoration, never removal.
/// Failed observation after mutation retains this same evidence for explicit retry.
///
/// Restoration evidence cannot be duplicated:
/// ```compile_fail
/// fn duplicate<K: bpfman_fs::XdpSwitchKernel>(receipt: bpfman_fs::XdpSwitch<K>) {
///     let copy = receipt.clone();
/// }
/// ```
#[must_use = "retain until publication succeeds or restoration completes"]
pub struct XdpSwitch<K: XdpSwitchKernel> {
    pin: Box<Entry>,
    outer: K::Outer,
    old: K::Target,
    new: K::Target,
    old_id: NonZeroU32,
    new_id: NonZeroU32,
    outer_id: NonZeroU32,
    key: XdpKey,
}

impl RuntimeWriter<'_> {
    /// Conditionally switch a durable outer link. None in an error means no
    /// mutation; Some retains restoration evidence after the target changed.
    pub fn switch_xdp<K: XdpSwitchKernel>(
        &self,
        kernel: &K,
        outer: &XdpOuter<K::Outer>,
        old: &XdpProgramPin,
        new: &XdpProgramPin,
    ) -> Result<XdpSwitch<K>, EffectFailure<Option<XdpSwitch<K>>, Error>> {
        let prepare = || -> Result<XdpSwitch<K>, Error> {
            let OuterState::Pinned {
                entry: outer_pin,
                id,
            } = &outer.state
            else {
                return Err(Failure::Unsafe("replacement requires a pinned outer link").into());
            };
            if old.key != new.key
                || old.revision.get().checked_add(1) != Some(new.revision.get())
                || old.id == new.id
                || outer_pin.name != outer_name(old.key)
            {
                return Err(Failure::Unsafe("inconsistent replacement revisions").into());
            }
            outer_pin.check_writer(self)?;
            old.entry.check_writer(self)?;
            new.entry.check_writer(self)?;
            let outer_owned = open_owned(outer_pin)?;
            let old_owned = open_owned(&old.entry)?;
            let new_owned = open_owned(&new.entry)?;
            let fd = kernel
                .outer_at(crate::PinSource(&proc_path(&outer_owned)))
                .map_err(Failure::Kernel)?;
            let info = fd.info().map_err(Failure::Kernel)?;
            if info.id != id.get()
                || info.program != old.id.get()
                || info.ifindex != old.key.ifindex.get()
            {
                return Err(
                    Failure::Unsafe("live outer target differs from expected revision").into(),
                );
            }
            let (old_info, old_target) = kernel
                .target_at(crate::PinSource(&proc_path(&old_owned)))
                .map_err(Failure::Kernel)?;
            let (new_info, new_target) = kernel
                .target_at(crate::PinSource(&proc_path(&new_owned)))
                .map_err(Failure::Kernel)?;
            if old_info.id != old.id.get()
                || new_info.id != new.id.get()
                || old_info.kind != crate::PinProgramKind::Xdp
                || new_info.kind != crate::PinProgramKind::Xdp
            {
                return Err(Failure::Unsafe("replacement program identity changed").into());
            }
            // Retain separate observation evidence, without duplicating removal ownership.
            let pin = Entry {
                root: outer_pin.root,
                parent: rustix::io::fcntl_dupfd_cloexec(&outer_pin.parent, 0)
                    .map_err(|e| io("retain switch parent", e))?,
                name: outer_pin.name.clone(),
                parent_path: outer_pin.parent_path.clone(),
                identity: outer_pin.identity,
                directory: false,
            };
            Ok(XdpSwitch {
                pin: Box::new(pin),
                outer: fd,
                old: old_target,
                new: new_target,
                old_id: old.id,
                new_id: new.id,
                outer_id: *id,
                key: old.key,
            })
        };
        let receipt = prepare().map_err(fail)?;
        kernel
            .replace_outer(&receipt.outer, &receipt.old, &receipt.new)
            .map_err(|e| fail(Failure::Kernel(e).into()))?;
        match receipt.verify(self, receipt.new_id) {
            Ok(()) => Ok(receipt),
            Err(cause) => Err(EffectFailure {
                cause,
                remaining: Some(receipt),
            }),
        }
    }

    /// Restore once, retaining evidence on every failure. Already-restored state
    /// is accepted on explicit retry; any unrelated target is refused.
    pub fn restore_xdp<K: XdpSwitchKernel>(
        &self,
        kernel: &K,
        receipt: XdpSwitch<K>,
    ) -> Result<(), EffectFailure<XdpSwitch<K>, Error>> {
        let result = (|| {
            receipt.pin.check_writer(self)?;
            open_owned(&receipt.pin)?;
            let info = receipt.outer.info().map_err(Failure::Kernel)?;
            if info.id != receipt.outer_id.get() || info.ifindex != receipt.key.ifindex.get() {
                return Err(Failure::Unsafe("outer link changed before restoration").into());
            }
            if info.program != receipt.old_id.get() {
                if info.program != receipt.new_id.get() {
                    return Err(Failure::Unsafe("unrelated live target blocks restoration").into());
                }
                kernel
                    .replace_outer(&receipt.outer, &receipt.new, &receipt.old)
                    .map_err(Failure::Kernel)?;
            }
            receipt.verify(self, receipt.old_id)
        })();
        result.map_err(|cause| EffectFailure {
            cause,
            remaining: receipt,
        })
    }
}

impl<K: XdpSwitchKernel> XdpSwitch<K> {
    fn verify(&self, writer: &RuntimeWriter<'_>, program: NonZeroU32) -> Result<(), Error> {
        self.pin.check_writer(writer)?;
        open_owned(&self.pin)?;
        let info = self.outer.info().map_err(Failure::Kernel)?;
        if info.id != self.outer_id.get()
            || info.program != program.get()
            || info.ifindex != self.key.ifindex.get()
        {
            return Err(Failure::Unsafe("outer link differs after target update").into());
        }
        Ok(())
    }
}
