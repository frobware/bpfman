//! Verify XDP extensions against an unpinned, one-slot dispatcher, as Go does.
//! Only the returned extension is published. Aya/kernel references keep its
//! verification target alive. Aya closes its target descriptor when the load
//! object is released; the kernel releases the target with the extension.

use crate::failure::LoadCause;
use aya::programs::{Extension, Program, Xdp};

pub(super) fn load(program: &mut Program, frags: bool) -> Result<(), LoadCause> {
    let mut config = test_config();
    config[3] = u8::from(frags);
    config[84..88].copy_from_slice(&(if frags { 32u32 } else { 0 }).to_ne_bytes());
    let mut dispatcher = aya::EbpfLoader::new()
        .override_global("conf", config.as_slice(), true)
        .load(dispatcher_bytes(frags))
        .map_err(|e| LoadCause::Kernel(Box::new(e)))?;
    let target: &mut Xdp = dispatcher
        .program_mut("xdp_dispatcher")
        .ok_or_else(|| LoadCause::Invalid("embedded XDP dispatcher is missing".into()))?
        .try_into()
        .map_err(|e| LoadCause::Program(Box::new(e)))?;
    target.load().map_err(|e| LoadCause::Program(Box::new(e)))?;
    let target = target
        .fd()
        .and_then(|fd| fd.try_clone().map_err(Into::into))
        .map_err(|e| LoadCause::Program(Box::new(e)))?;
    let extension: &mut Extension = program
        .try_into()
        .map_err(|e| LoadCause::Program(Box::new(e)))?;
    extension
        .load(target, "prog0")
        .map_err(|e| LoadCause::Program(Box::new(e)))
}

// C ABI in dispatcher/bpf/xdp_dispatcher_v2.bpf.c: four bytes followed
// by three arrays of ten native-endian u32 values. No unsafe Pod conversion.
fn test_config() -> [u8; 124] {
    let mut config = [0; 124];
    config[..4].copy_from_slice(&[236, 2, 1, 0]);
    for priority in config[44..84].chunks_exact_mut(4) {
        priority.copy_from_slice(&50u32.to_ne_bytes());
    }
    config
}

// Each embedded variant comes from the same shared source. Only its section differs.
pub(super) fn dispatcher_bytes(frags: bool) -> &'static [u8] {
    if frags {
        aya::include_bytes_aligned!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../dispatcher/xdp_dispatcher_v2_frags.bpf.o"
        ))
    } else {
        aya::include_bytes_aligned!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../dispatcher/xdp_dispatcher_v2.bpf.o"
        ))
    }
}

// TC uses its own dispatcher and skb context; verification never attaches traffic.
pub(super) fn load_tc(program: &mut Program) -> Result<(), LoadCause> {
    let config = bpfman_model::tc_config(bpfman_model::TcProceedOn::default());
    let mut dispatcher = aya::EbpfLoader::new()
        .override_global("CONFIG", config.as_slice(), true)
        .load(tc_dispatcher_bytes())
        .map_err(|e| LoadCause::Kernel(Box::new(e)))?;
    let target: &mut aya::programs::SchedClassifier = dispatcher
        .program_mut("tc_dispatcher")
        .ok_or_else(|| LoadCause::Invalid("embedded TC dispatcher is missing".into()))?
        .try_into()
        .map_err(|e| LoadCause::Program(Box::new(e)))?;
    target.load().map_err(|e| LoadCause::Program(Box::new(e)))?;
    let fd = target
        .fd()
        .and_then(|fd| fd.try_clone().map_err(Into::into))
        .map_err(|e| LoadCause::Program(Box::new(e)))?;
    let extension: &mut Extension = program
        .try_into()
        .map_err(|e| LoadCause::Program(Box::new(e)))?;
    extension
        .load(fd, "prog0")
        .map_err(|e| LoadCause::Program(Box::new(e)))
}

pub(super) fn tc_dispatcher_bytes() -> &'static [u8] {
    aya::include_bytes_aligned!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../dispatcher/tc_dispatcher.bpf.o"
    ))
}
