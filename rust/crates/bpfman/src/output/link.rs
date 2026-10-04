use crate::cli::OutputFormat;
use bpfman_model::{LinkDetails, LinkState, ObservedLink, StoredLink};
use serde_json::{Value, json};
use std::io::{self, Write};

fn record(link: &StoredLink) -> Value {
    let LinkDetails::Tracepoint(target) = &link.details;
    let kernel = match link.state {
        LinkState::Pending => None,
        LinkState::Attached { kernel_id } => Some(kernel_id.get()),
    };

    json!({
        "id": link.id.get(), "program_id": link.program_id.get(),
        "kernel_link_id": kernel, "kind": "tracepoint", "pin_path": link.pin_path,
        "details": { "group": target.group(), "name": target.name() },
        "created_at": link.created_at, "metadata": link.metadata,
    })
}

pub(crate) fn link(
    out: &mut impl Write,
    link: &ObservedLink,
    format: OutputFormat,
) -> io::Result<()> {
    if format == OutputFormat::Json {
        // These other-kind fields are zero-shaped in Go's kernel wire format.
        // The runtime currently admits only standalone perf-event links.
        let kernel = link.kernel.as_ref().map(|k| {
            json!({
                "id": k.id.get(), "program_id": k.program_id.get(), "link_type": "perf_event",
                "attach_type": "", "ifindex": 0, "target_obj_id": 0, "target_btf_id": 0,
                "cgroup_id": 0, "netns_ino": 0, "netfilter_pf": 0, "netfilter_hooknum": 0,
                "netfilter_priority": 0, "netfilter_flags": 0, "kprobe_multi_count": 0,
                "kprobe_multi_flags": 0, "kprobe_multi_missed": 0, "kprobe_address": 0,
                "kprobe_missed": 0,
            })
        });
        return super::write_json(
            out,
            &json!({
                "record": record(&link.record),
                "status": { "kernel": kernel, "kernel_seen": link.kernel.is_some(), "pin_present": link.pin_present },
            }),
        );
    }

    let LinkDetails::Tracepoint(target) = &link.record.details;
    let kernel = match link.record.state {
        LinkState::Pending => "<none>".into(),
        LinkState::Attached { kernel_id } => kernel_id.to_string(),
    };
    writeln!(
        out,
        "Link ID: {}\nKernel Link ID: {kernel}\nKind: tracepoint\nProgram ID: {}\nAttachment: {target}\nPin Path: {}\nKernel Seen: {}\nPin Present: {}",
        link.record.id,
        link.record.program_id,
        link.record.pin_path,
        link.kernel.is_some(),
        link.pin_present
    )
}

pub(crate) fn links(
    out: &mut impl Write,
    links: &[StoredLink],
    format: OutputFormat,
) -> io::Result<()> {
    if format == OutputFormat::Json {
        return super::write_json(
            out,
            &json!({"links": links.iter().map(record).collect::<Vec<_>>() }),
        );
    }
    if links.is_empty() {
        return Ok(());
    }

    writeln!(out, "LINK ID  KERNEL LINK ID  KIND  PROGRAM ID  ATTACHMENT")?;
    for link in links {
        let LinkDetails::Tracepoint(target) = &link.details;
        let kernel = match link.state {
            LinkState::Pending => "<none>".into(),
            LinkState::Attached { kernel_id } => kernel_id.to_string(),
        };
        writeln!(
            out,
            "{}  {kernel}  tracepoint  {}  {target}",
            link.id, link.program_id
        )?;
    }

    Ok(())
}
