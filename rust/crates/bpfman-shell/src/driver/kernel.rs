use std::num::NonZeroU32;

use bpfman_shell_runtime::{Argument, CommandResponse, Value};

pub(super) fn linkinfo(arguments: &[Argument]) -> CommandResponse {
    match observe_link(arguments) {
        Ok(value) => CommandResponse::success(value),
        Err(stderr) => CommandResponse {
            exit_code: 1,
            stderr,
            ..CommandResponse::success(Value::Absent)
        },
    }
}

fn observe_link(arguments: &[Argument]) -> Result<Value, String> {
    let [mode, id] = arguments else {
        return Err("linkinfo: usage: linkinfo id N".to_owned());
    };
    if super::argument(mode)? != "id" {
        return Err("linkinfo: usage: linkinfo id N".to_owned());
    }
    let raw = super::argument(id)?;
    let id = raw
        .parse::<NonZeroU32>()
        .map_err(|error| format!("linkinfo: id {raw:?}: {error}"))?;
    let identity =
        bpfman_kernel_aya::link_identity(id).map_err(|error| format!("linkinfo: {error}"))?;
    let kind = match identity.type_code {
        0 => "unspec",
        1 => "raw_tracepoint",
        2 => "tracing",
        3 => "cgroup",
        4 => "iter",
        5 => "netns",
        6 => "xdp",
        7 => "perf_event",
        8 => "kprobe_multi",
        9 => "struct_ops",
        10 => "netfilter",
        11 => "tcx",
        12 => "uprobe_multi",
        13 => "netkit",
        unknown => return link_value(identity, unknown.to_string()),
    };
    link_value(identity, kind.to_owned())
}

fn link_value(
    identity: bpfman_kernel_aya::KernelLinkIdentity,
    kind: String,
) -> Result<Value, String> {
    super::bpfman::project(serde_json::json!({
        "id": identity.id.get(),
        "prog_id": identity.program_id,
        "type": kind,
    }))
}
