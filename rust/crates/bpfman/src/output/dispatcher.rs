use bpfman_model::{LinkState, XdpDispatcherSnapshot};
use std::io::{self, Write};

pub(crate) fn dispatcher(out: &mut impl Write, snapshot: &XdpDispatcherSnapshot) -> io::Result<()> {
    let first = snapshot
        .members()
        .first()
        .ok_or_else(|| io::Error::other("empty XDP snapshot"))?;
    let d = &first.details;
    let members: Vec<_> = snapshot
        .members()
        .iter()
        .map(|snapshot| {
            let member = &snapshot.member;
            let d = &snapshot.details;
            let kernel = match member.state {
                LinkState::Attached { kernel_id } => Some(kernel_id.get()),
                LinkState::Pending => None,
            };
            serde_json::json!({
                "program_id": member.program_id.get(),
                "program_name": snapshot.program_name.as_str(),
                "prog_pin_path": snapshot.program_pin_path,
                "link_id": member.id.get(),
                "kernel_link_id": kernel,
                "link_pin_path": member.pin_path,
                "position": d.slot.index(),
                "priority": d.priority,
                "proceed_on": d.proceed_on.mask(),
                "ifname": d.interface.as_str(),
                "metadata": member.metadata,
            })
        })
        .collect();
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
                "kernel_link_id": first.outer_link_id.get(),
                "netns_path": "",
            },
            "members": members,
        }),
    )
}

pub(crate) fn dispatchers(
    out: &mut impl Write,
    snapshots: &[XdpDispatcherSnapshot],
) -> io::Result<()> {
    let entries: Vec<_> = snapshots
        .iter()
        .map(|s| {
            let first = s
                .members()
                .first()
                .ok_or_else(|| io::Error::other("empty XDP snapshot"))?;
            let d = &first.details;
            Ok(serde_json::json!({
                "key": {
                    "type": "xdp",
                    "nsid": d.key.nsid.get(),
                    "ifindex": d.key.ifindex.get(),
                },
                "revision": d.revision.get(),
                "runtime": {
                    "program_id": d.dispatcher_id.get(),
                    "kernel_link_id": first.outer_link_id.get(),
                    "netns_path": "",
                },
                "member_count": s.members().len(),
            }))
        })
        .collect::<io::Result<_>>()?;
    super::write_json(out, &serde_json::json!({"dispatchers": entries}))
}
