use bpfman_model::{LinkState, XdpSnapshot};
use std::io::{self, Write};

pub(crate) fn dispatcher(out: &mut impl Write, snapshot: &XdpSnapshot) -> io::Result<()> {
    let member = &snapshot.member;
    let d = &snapshot.details;
    let kernel = match member.state {
        LinkState::Attached { kernel_id } => Some(kernel_id.get()),
        LinkState::Pending => None,
    };
    super::write_json(
        out,
        &serde_json::json!({
            "key": {
                "type": "xdp",
                "nsid": d.key.nsid.get(),
                "ifindex": d.key.ifindex.get(),
            },
            "revision": d.revision.get(),
            "runtime": {
                "program_id": d.dispatcher_id.get(),
                "kernel_link_id": snapshot.outer_link_id.get(),
                "netns_path": "",
            },
            "members": [{
                "program_id": member.program_id.get(),
                "program_name": snapshot.program_name.as_str(),
                "prog_pin_path": snapshot.program_pin_path,
                "link_id": member.id.get(),
                "kernel_link_id": kernel,
                "link_pin_path": member.pin_path,
                "position": 0,
                "priority": d.priority,
                "proceed_on": d.proceed_on.mask(),
                "ifname": d.interface.as_str(),
                "metadata": member.metadata,
            }],
        }),
    )
}
