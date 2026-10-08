use crate::cli::OutputFormat;
use bpfman_model::{LinkDetails, LinkState, ObservedLink, StoredLink};
use serde_json::{Value, json};
use std::io::{self, Write};

fn record(link: &StoredLink) -> Value {
    let (kind, details) = match &link.details {
        LinkDetails::Tracepoint(target) => (
            "tracepoint",
            json!({"group": target.group(), "name": target.name()}),
        ),
        LinkDetails::Tc(d) => (
            "tc",
            json!({
                "interface":d.interface.as_str(), "ifindex":d.key.ifindex.get(), "direction":"ingress", "priority":d.priority,
                "position":d.slot.index(), "proceed_on":(-1..=30).filter(|code| d.proceed_on.mask() & (1 << (code+1)) != 0).collect::<Vec<i32>>(),
                "netns":d.netns.as_str(), "nsid":d.key.nsid.get(), "dispatcher_id":d.dispatcher_id.get(), "revision":d.revision.get(),
                "filter_priority":d.filter_priority, "filter_handle":d.filter_handle.get(),
            }),
        ),
        LinkDetails::Xdp(d) => (
            "xdp",
            json!({
                "interface": d.interface.as_str(),
                "ifindex": d.key.ifindex.get(),
                "priority": d.priority,
                "position": d.slot.index(),
                "proceed_on": (0..32)
                    .filter(|code| d.proceed_on.mask() & (1 << code) != 0)
                    .collect::<Vec<_>>(),
                "netns": d.netns.as_str(),
                "nsid": d.key.nsid.get(),
                "dispatcher_id": d.dispatcher_id.get(),
                "revision": d.revision.get(),
            }),
        ),
    };
    let kernel = match link.state {
        LinkState::Pending => None,
        LinkState::Attached { kernel_id } => Some(kernel_id.get()),
    };

    json!({
        "id": link.id.get(), "program_id": link.program_id.get(),
        "kernel_link_id": kernel, "kind": kind, "pin_path": link.pin_path,
        "details": details,
        "created_at": link.created_at, "metadata": link.metadata,
    })
}

pub(crate) fn link(
    out: &mut impl Write,
    link: &ObservedLink,
    format: OutputFormat,
) -> io::Result<()> {
    if format == OutputFormat::Json {
        return super::write_json(out, &observed(link));
    }

    let (kind, target) = attachment(&link.record.details);
    let kernel = match link.record.state {
        LinkState::Pending => "<none>".into(),
        LinkState::Attached { kernel_id } => kernel_id.to_string(),
    };
    writeln!(
        out,
        "Link ID: {}\nKernel Link ID: {kernel}\nKind: {kind}\nProgram ID: {}\nAttachment: {target}\nPin Path: {}\nKernel Seen: {}\nPin Present: {}",
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
        let (kind, target) = attachment(&link.details);
        let kernel = match link.state {
            LinkState::Pending => "<none>".into(),
            LinkState::Attached { kernel_id } => kernel_id.to_string(),
        };
        writeln!(
            out,
            "{}  {kernel}  {kind}  {}  {target}",
            link.id, link.program_id
        )?;
    }

    Ok(())
}

pub(super) fn observed(link: &ObservedLink) -> Value {
    // Type-specific fields come from actual kernel link observations.
    let kernel = link.kernel.as_ref().map(|k| {
        let (kind, attach, target, btf) = match k.details {
            bpfman_model::KernelLinkDetails::PerfEvent => ("perf_event", String::new(), 0, 0),
            bpfman_model::KernelLinkDetails::Tracing {
                attach_type,
                target_obj_id,
                target_btf_id,
            } => (
                "tracing",
                attach_type.to_string(),
                target_obj_id,
                target_btf_id,
            ),
        };
        json!({
            "id": k.id.get(), "program_id": k.program_id.get(), "link_type": kind,
            "attach_type": attach, "ifindex": 0, "target_obj_id": target, "target_btf_id": btf,
            "cgroup_id": 0, "netns_ino": 0, "netfilter_pf": 0, "netfilter_hooknum": 0,
            "netfilter_priority": 0, "netfilter_flags": 0, "kprobe_multi_count": 0,
            "kprobe_multi_flags": 0, "kprobe_multi_missed": 0, "kprobe_address": 0,
            "kprobe_missed": 0,
        })
    });
    json!({
        "record": record(&link.record),
        "status": { "kernel": kernel, "kernel_seen": link.kernel.is_some(), "pin_present": link.pin_present },
    })
}

pub(super) fn attachment(details: &LinkDetails) -> (&'static str, String) {
    match details {
        LinkDetails::Tc(d) => ("tc", format!("{}:ingress", d.interface.as_str())),
        LinkDetails::Tracepoint(t) => ("tracepoint", t.to_string()),
        LinkDetails::Xdp(d) => ("xdp", format!("{}:xdp", d.interface.as_str())),
    }
}
