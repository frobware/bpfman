//! Stable inspection and attachment points in the shell ELF.

use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) const SYMBOL: &str = "bpfman_shell_uprobe_call_malloc";
pub(crate) const DOTTED_SYMBOL: &str =
    "github.com/bpfman/bpfman/cmd/bpfman-shell/fixturemode.GoUprobeTarget";

static VISITS: AtomicU64 = AtomicU64::new(0);

pub(crate) fn fire_once() {
    allocation_target();
    dotted_target();
}

#[inline(never)]
#[allow(unsafe_code)]
// SAFETY: the shell reserves this unique ELF symbol for its fixture.
#[unsafe(export_name = "bpfman_shell_uprobe_call_malloc")]
fn allocation_target() {
    drop(std::hint::black_box(Box::new(0_u8)));
}

#[inline(never)]
#[allow(unsafe_code)]
// SAFETY: this unique compatibility name supplies the dotted probe target.
#[unsafe(export_name = "github.com/bpfman/bpfman/cmd/bpfman-shell/fixturemode.GoUprobeTarget")]
fn dotted_target() {
    VISITS.fetch_add(1, Ordering::Relaxed);
}
