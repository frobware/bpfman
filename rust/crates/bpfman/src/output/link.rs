use crate::cli::OutputFormat;
use bpfman_model::{LinkDetails, LinkState, ObservedLink, StoredLink, StoredProgramSummary};
use serde_json::{Value, json};
use std::io::{self, Write};

fn record(link: &StoredLink) -> Value {
    let (kind, details) = match &link.details {
        LinkDetails::Uprobe(d) => (
            if d.retprobe() { "uretprobe" } else { "uprobe" },
            json!({"target": d.target(), "fn_name": d.function().unwrap_or_default(), "offset": d.offset(), "pid": d.pid().map_or(0, std::num::NonZeroU32::get), "container_pid": d.container_pid().map_or(0, std::num::NonZeroU32::get), "retprobe": d.retprobe()}),
        ),
        LinkDetails::Tracing(d) => (
            d.kind.program_type().as_str(),
            match d.kind {
                bpfman_model::TracingKind::Lsm => json!({"hook_name": d.function.as_str()}),
                _ => json!({"fn_name": d.function.as_str()}),
            },
        ),
        LinkDetails::Kprobe(d) => (
            if d.retprobe { "kretprobe" } else { "kprobe" },
            json!({"fn_name": d.function.as_str(), "offset": d.offset, "retprobe": d.retprobe}),
        ),
        LinkDetails::Tracepoint(target) => (
            "tracepoint",
            json!({"group": target.group(), "name": target.name()}),
        ),
        LinkDetails::Tcx(d) => (
            "tcx",
            json!({"interface":d.interface.as_str(),"ifindex":d.key.ifindex.get(),"direction":d.key.direction.as_str(),"priority":d.priority,"position":d.position,"netns":d.netns.as_str(),"nsid":d.key.nsid.get()}),
        ),
        LinkDetails::Tc(d) => (
            "tc",
            json!({
                "interface":d.interface.as_str(), "ifindex":d.key.ifindex.get(), "direction":d.key.direction.as_str(), "priority":d.priority,
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
    program_name: &str,
    format: OutputFormat,
) -> io::Result<()> {
    if format == OutputFormat::Json {
        return super::write_json(out, &observed(link));
    }

    use super::detail::{field, render, section, sort};
    let r = &link.record;
    let mut spec = vec![
        field(
            "Kernel Link ID",
            match r.state {
                LinkState::Pending => "None".into(),
                LinkState::Attached { kernel_id } => kernel_id.to_string(),
            },
        ),
        field(
            "Metadata",
            if r.metadata.is_empty() {
                "None".into()
            } else {
                r.metadata
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            },
        ),
        field("Pin Path", or_none(&r.pin_path)),
        field("Program ID", r.program_id),
        field("Type", r.details.kind()),
    ];
    if !program_name.is_empty() {
        spec.push(field("BPF Function", program_name));
    }
    if !r.created_at.is_empty() && r.created_at != "0001-01-01T00:00:00Z" {
        // Stored timestamps are validated RFC3339Nano; Go text uses RFC3339.
        let created = if let Some((seconds, fraction)) = r.created_at.split_once('.') {
            let zone = fraction.trim_start_matches(|c: char| c.is_ascii_digit());
            format!("{seconds}{zone}")
        } else {
            r.created_at.clone()
        };
        spec.push(field("Created At", created));
    }
    match &r.details {
        LinkDetails::Tracing(d) => spec.push(field(
            if d.kind == bpfman_model::TracingKind::Lsm {
                "LSM Hook"
            } else {
                "Target Function"
            },
            d.function.as_str(),
        )),
        LinkDetails::Kprobe(d) => {
            spec.push(field("Attach Type", r.details.kind()));
            spec.push(field("Target Function", d.function.as_str()));
            if d.offset != 0 {
                spec.push(field("Target Offset", d.offset));
            }
        }
        LinkDetails::Uprobe(d) => {
            spec.push(field("Attach Type", r.details.kind()));
            if let Some(pid) = d.pid() {
                spec.push(field("PID", pid));
            }
            spec.push(field("Target", d.target()));
            spec.push(field("Target Function", d.function().unwrap_or_default()));
            if d.offset() != 0 {
                spec.push(field("Target Offset", d.offset()));
            }
        }
        LinkDetails::Tracepoint(t) => spec.push(field("Tracepoint", t)),
        LinkDetails::Xdp(d) => spec.extend([
            field("Interface", d.interface.as_str()),
            field("Network Namespace", or_none(d.netns.as_str())),
            field("Position", d.slot.index()),
            field("Priority", d.priority),
            field(
                "Proceed On",
                super::dispatcher::proceed_on(d.proceed_on.mask(), true),
            ),
        ]),
        LinkDetails::Tc(d) => spec.extend([
            field("Direction", d.key.direction),
            field("Interface", d.interface.as_str()),
            field("Network Namespace", or_none(d.netns.as_str())),
            field("Position", d.slot.index()),
            field("Priority", d.priority),
            field(
                "Proceed On",
                super::dispatcher::proceed_on(d.proceed_on.mask(), false),
            ),
        ]),
        LinkDetails::Tcx(d) => spec.extend([
            field("Direction", d.key.direction),
            field("Interface", d.interface.as_str()),
            field("Network Namespace", or_none(d.netns.as_str())),
            field("Position", d.position),
            field("Priority", d.priority),
        ]),
    }
    sort(&mut spec);
    writeln!(out, "Link ID: {}", r.id)?;
    render(
        out,
        &[
            section("Spec", spec),
            section(
                "Status",
                vec![
                    field("Kernel Seen", link.kernel.is_some()),
                    field("Pin Present", link.pin_present),
                ],
            ),
        ],
        1,
    )
}

fn or_none(value: &str) -> &str {
    if value.is_empty() { "None" } else { value }
}

pub(crate) fn links(
    out: &mut impl Write,
    links: &[StoredLink],
    programs: &[StoredProgramSummary],
    format: OutputFormat,
    quiet: bool,
) -> io::Result<()> {
    if quiet {
        for link in links {
            writeln!(out, "{}", link.id)?;
        }
        return Ok(());
    }
    if format == OutputFormat::Json {
        return super::write_json(
            out,
            &json!({"links": links.iter().map(record).collect::<Vec<_>>() }),
        );
    }
    if links.is_empty() {
        return Ok(());
    }

    let programs = programs
        .iter()
        .map(|p| (p.id(), p))
        .collect::<std::collections::BTreeMap<_, _>>();
    let with_application = links.iter().any(|l| {
        programs
            .get(&l.program_id)
            .is_some_and(|p| !p.application().is_empty())
    });
    let mut headers = vec!["LINK ID", "KERNEL LINK ID", "KIND", "PROGRAM ID"];
    if with_application {
        headers.push("APPLICATION");
    }
    headers.extend(["FUNCTION NAME", "ATTACHMENT"]);
    let rows = links
        .iter()
        .map(|link| {
            let kernel = match link.state {
                LinkState::Pending => "<none>".into(),
                LinkState::Attached { kernel_id } => kernel_id.to_string(),
            };
            let program = programs.get(&link.program_id);
            let mut row = vec![
                link.id.to_string(),
                kernel,
                link.details.kind().to_string(),
                link.program_id.to_string(),
            ];
            if with_application {
                row.push(program.map_or("", |p| p.application()).into());
            }
            row.extend([
                program
                    .map_or("<none>", |p| {
                        if p.name().is_empty() {
                            "<none>"
                        } else {
                            p.name()
                        }
                    })
                    .into(),
                summary(&link.details),
            ]);
            row
        })
        .collect::<Vec<_>>();
    super::dispatcher::table(out, "", &headers, &rows)
}

fn summary(details: &LinkDetails) -> String {
    let network = |interface: &str, direction: &str, position, netns: &str| {
        let mut value = format!("{interface}{direction} pos-{position}");
        if !netns.is_empty() {
            value.push_str(&format!(" netns={netns}"));
        }
        value
    };
    match details {
        LinkDetails::Xdp(d) => network(d.interface.as_str(), "", d.slot.index(), d.netns.as_str()),
        LinkDetails::Tc(d) => network(
            d.interface.as_str(),
            &format!(" {}", d.key.direction),
            d.slot.index(),
            d.netns.as_str(),
        ),
        LinkDetails::Tcx(d) => network(
            d.interface.as_str(),
            &format!(" {}", d.key.direction),
            d.position,
            d.netns.as_str(),
        ),
        LinkDetails::Kprobe(d) if d.offset == 0 => d.function.as_str().to_owned(),
        LinkDetails::Kprobe(d) => format!("{}+{}", d.function.as_str(), d.offset),
        LinkDetails::Uprobe(d) => match d.function() {
            Some(function) => format!("{} {function}", d.target()),
            None => format!("{} +{}", d.target(), d.offset()),
        },
        LinkDetails::Tracepoint(t) => t.to_string(),
        LinkDetails::Tracing(d) => d.function.as_str().to_owned(),
    }
}

pub(super) fn observed(link: &ObservedLink) -> Value {
    // Type-specific fields come from actual kernel link observations.
    let kernel = link.kernel.as_ref().map(|k| {
        let (kind, attach, target, btf, ifindex) = match k.details {
            bpfman_model::KernelLinkDetails::PerfEvent => ("perf_event", String::new(), 0, 0, 0),
            bpfman_model::KernelLinkDetails::Tracing {
                attach_type,
                target_obj_id,
                target_btf_id,
            } => (
                "tracing",
                attach_type.to_string(),
                target_obj_id,
                target_btf_id,
                0,
            ),
            bpfman_model::KernelLinkDetails::Tcx {ifindex,direction} => ("tcx",match direction {bpfman_model::TcDirection::Ingress=>"tcx_ingress", bpfman_model::TcDirection::Egress=>"tcx_egress"}.into(),0,0,ifindex),
        };
        json!({
            "id": k.id.get(), "program_id": k.program_id.get(), "link_type": kind,
            "attach_type": attach, "ifindex": ifindex, "target_obj_id": target, "target_btf_id": btf,
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
        LinkDetails::Tcx(d) => (
            "tcx",
            format!("{}:{}", d.interface.as_str(), d.key.direction),
        ),
        LinkDetails::Tc(d) => (
            "tc",
            format!("{}:{}", d.interface.as_str(), d.key.direction),
        ),
        LinkDetails::Uprobe(d) => (
            if d.retprobe() { "uretprobe" } else { "uprobe" },
            format!(
                "{}:{}+{}",
                d.target(),
                d.function().unwrap_or_default(),
                d.offset()
            ),
        ),
        LinkDetails::Tracing(d) => (d.kind.program_type().as_str(), d.function.as_str().into()),
        LinkDetails::Kprobe(d) => (
            if d.retprobe { "kretprobe" } else { "kprobe" },
            format!("{}+{}", d.function.as_str(), d.offset),
        ),
        LinkDetails::Tracepoint(t) => ("tracepoint", t.to_string()),
        LinkDetails::Xdp(d) => ("xdp", format!("{}:xdp", d.interface.as_str())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        num::{NonZeroU32, NonZeroU64},
    };

    #[test]
    fn pending_orphan_text_keeps_intent_and_missing_observations()
    -> Result<(), Box<dyn std::error::Error>> {
        let record = StoredLink {
            id: NonZeroU64::new(7).ok_or("id")?,
            program_id: NonZeroU32::new(42).ok_or("program")?,
            details: LinkDetails::Tracepoint("syscalls/sys_enter_kill".parse()?),
            state: LinkState::Pending,
            pin_path: String::new(),
            metadata: BTreeMap::new(),
            created_at: "2026-10-18T12:34:56.123456789+01:00".into(),
        };
        let mut out = Vec::new();
        links(
            &mut out,
            std::slice::from_ref(&record),
            &[],
            OutputFormat::Text,
            false,
        )?;
        let text = String::from_utf8(out)?;
        assert!(text.contains("FUNCTION NAME"));
        assert!(!text.contains("APPLICATION"));
        assert_eq!(
            text.lines()
                .nth(1)
                .ok_or("row")?
                .split_whitespace()
                .collect::<Vec<_>>(),
            [
                "7",
                "<none>",
                "tracepoint",
                "42",
                "<none>",
                "syscalls/sys_enter_kill"
            ]
        );
        let observed = ObservedLink {
            record,
            kernel: None,
            pin_present: false,
        };
        let mut out = Vec::new();
        link(&mut out, &observed, "", OutputFormat::Text)?;
        let text = String::from_utf8(out)?;
        for field in [
            "Created At: 2026-10-18T12:34:56+01:00",
            "Kernel Link ID: None",
            "Metadata: None",
            "Pin Path: None",
            "Kernel Seen: false",
            "Pin Present: false",
        ] {
            let normalized = text
                .lines()
                .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
                .collect::<Vec<_>>();
            assert!(normalized.iter().any(|line| line == field), "{text}");
        }
        assert!(!text.contains("BPF Function"));
        let mut out = Vec::new();
        links(&mut out, &[observed.record], &[], OutputFormat::Json, true)?;
        assert_eq!(out, b"7\n");
        Ok(())
    }
}
