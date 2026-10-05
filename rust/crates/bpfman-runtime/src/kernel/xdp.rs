//! Verify XDP extensions against an unpinned, one-slot dispatcher, as Go does.
//! Only the returned extension is published. Aya/kernel references keep its
//! verification target alive. Aya closes its target descriptor when the load
//! object is released; the kernel releases the target with the extension.

use crate::load_error::LoadCause;
use aya::programs::{Extension, Program, Xdp};

pub(super) fn load(program: &mut Program) -> Result<(), LoadCause> {
    let config = test_config();
    let mut dispatcher = aya::EbpfLoader::new()
        .override_global("conf", config.as_slice(), true)
        .load(aya::include_bytes_aligned!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../dispatcher/xdp_dispatcher_v2.bpf.o"
        )))
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
