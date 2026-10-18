use bpfman_model::{LinkState, TcDispatcherSnapshot, XdpDispatcherSnapshot};
use serde::Serialize;
use std::io::{self, Write};

use crate::cli::OutputFormat;

#[derive(Clone, Copy, Serialize)]
struct Key {
    #[serde(rename = "type")]
    kind: &'static str,
    nsid: u64,
    ifindex: u32,
}

#[derive(Clone, Copy, Serialize)]
#[serde(untagged)]
enum Runtime<'a> {
    Xdp {
        program_id: u32,
        kernel_link_id: u32,
        netns_path: &'a str,
    },
    Tc {
        program_id: u32,
        filter_handle: u32,
        priority: u16,
        netns_path: &'a str,
    },
}

impl Runtime<'_> {
    fn cells(self) -> [String; 5] {
        let (program, link, priority, handle, netns) = match self {
            Self::Xdp {
                program_id,
                kernel_link_id,
                netns_path,
            } => (
                program_id,
                kernel_link_id.to_string(),
                "-".into(),
                "-".into(),
                netns_path,
            ),
            Self::Tc {
                program_id,
                filter_handle,
                priority,
                netns_path,
            } => (
                program_id,
                "-".into(),
                priority.to_string(),
                format!("{filter_handle:#x}"),
                netns_path,
            ),
        };
        [
            program.to_string(),
            link,
            priority,
            handle,
            if netns.is_empty() { "-" } else { netns }.into(),
        ]
    }
}

#[derive(Serialize)]
struct Member<'a> {
    program_id: u32,
    program_name: &'a str,
    prog_pin_path: &'a str,
    link_id: u64,
    kernel_link_id: Option<u32>,
    link_pin_path: &'a str,
    position: usize,
    priority: u32,
    proceed_on: u32,
    ifname: &'a str,
    metadata: &'a std::collections::BTreeMap<String, String>,
}

#[derive(Serialize)]
struct Snapshot<'a> {
    key: Key,
    revision: u32,
    runtime: Runtime<'a>,
    members: Vec<Member<'a>>,
}

#[derive(Serialize)]
struct Summary<'a> {
    key: Key,
    revision: u32,
    runtime: Runtime<'a>,
    member_count: usize,
}

impl<'a> From<Snapshot<'a>> for Summary<'a> {
    fn from(snapshot: Snapshot<'a>) -> Self {
        Self {
            key: snapshot.key,
            revision: snapshot.revision,
            runtime: snapshot.runtime,
            member_count: snapshot.members.len(),
        }
    }
}

pub(crate) fn dispatcher(
    out: &mut impl Write,
    snapshot: &XdpDispatcherSnapshot,
    format: OutputFormat,
) -> io::Result<()> {
    render(out, &xdp_snapshot(snapshot)?, format)
}

pub(crate) fn tc_dispatcher(
    out: &mut impl Write,
    snapshot: &TcDispatcherSnapshot,
    format: OutputFormat,
) -> io::Result<()> {
    render(out, &tc_snapshot(snapshot)?, format)
}

pub(crate) fn dispatchers(
    out: &mut impl Write,
    snapshots: &[XdpDispatcherSnapshot],
    tc: &[TcDispatcherSnapshot],
    format: OutputFormat,
) -> io::Result<()> {
    let mut entries = snapshots
        .iter()
        .map(xdp_snapshot)
        .map(|s| s.map(Summary::from))
        .collect::<io::Result<Vec<_>>>()?;
    for snapshot in tc {
        entries.push(tc_snapshot(snapshot)?.into());
    }
    if format == OutputFormat::Json {
        return super::write_json(out, &serde_json::json!({"dispatchers": entries}));
    }
    if entries.is_empty() {
        return Ok(());
    }
    let headers = [
        "TYPE",
        "NSID",
        "IFINDEX",
        "REVISION",
        "PROGRAM ID",
        "KERNEL LINK ID",
        "PRIORITY",
        "HANDLE",
        "MEMBERS",
        "NETNS",
    ];
    let rows = entries
        .iter()
        .map(|s| {
            let [program, link, priority, handle, netns] = s.runtime.cells();
            vec![
                s.key.kind.into(),
                s.key.nsid.to_string(),
                s.key.ifindex.to_string(),
                s.revision.to_string(),
                program,
                link,
                priority,
                handle,
                s.member_count.to_string(),
                netns,
            ]
        })
        .collect::<Vec<_>>();
    table(out, "", &headers, &rows)
}

fn xdp_snapshot(snapshot: &XdpDispatcherSnapshot) -> io::Result<Snapshot<'_>> {
    let first = snapshot
        .members()
        .first()
        .ok_or_else(|| io::Error::other("empty XDP snapshot"))?;
    let d = &first.details;
    Ok(Snapshot {
        key: Key {
            kind: "xdp",
            nsid: d.key.nsid.get(),
            ifindex: d.key.ifindex.get(),
        },
        revision: d.revision.get(),
        runtime: Runtime::Xdp {
            program_id: d.dispatcher_id.get(),
            kernel_link_id: first.outer_link_id.get(),
            netns_path: d.netns.as_str(),
        },
        members: snapshot
            .members()
            .iter()
            .map(|s| Member {
                program_id: s.member.program_id.get(),
                program_name: s.program_name.as_str(),
                prog_pin_path: &s.program_pin_path,
                link_id: s.member.id.get(),
                kernel_link_id: kernel_link_id(s.member.state),
                link_pin_path: &s.member.pin_path,
                position: s.details.slot.index(),
                priority: s.details.priority,
                proceed_on: s.details.proceed_on.mask(),
                ifname: s.details.interface.as_str(),
                metadata: &s.member.metadata,
            })
            .collect(),
    })
}

fn tc_snapshot(snapshot: &TcDispatcherSnapshot) -> io::Result<Snapshot<'_>> {
    let first = snapshot
        .members()
        .first()
        .ok_or_else(|| io::Error::other("empty TC snapshot"))?;
    let d = &first.details;
    Ok(Snapshot {
        key: Key {
            kind: d.key.direction.dispatcher_type(),
            nsid: d.key.nsid.get(),
            ifindex: d.key.ifindex.get(),
        },
        revision: d.revision.get(),
        runtime: Runtime::Tc {
            program_id: d.dispatcher_id.get(),
            filter_handle: d.filter_handle.get(),
            priority: d.filter_priority,
            netns_path: d.netns.as_str(),
        },
        members: snapshot
            .members()
            .iter()
            .map(|s| Member {
                program_id: s.member.program_id.get(),
                program_name: s.program_name.as_str(),
                prog_pin_path: &s.program_pin_path,
                link_id: s.member.id.get(),
                kernel_link_id: kernel_link_id(s.member.state),
                link_pin_path: &s.member.pin_path,
                position: s.details.slot.index(),
                priority: s.details.priority,
                proceed_on: s.details.proceed_on.mask(),
                ifname: s.details.interface.as_str(),
                metadata: &s.member.metadata,
            })
            .collect(),
    })
}

fn kernel_link_id(state: LinkState) -> Option<u32> {
    match state {
        LinkState::Attached { kernel_id } => Some(kernel_id.get()),
        LinkState::Pending => None,
    }
}

fn render(out: &mut impl Write, snapshot: &Snapshot<'_>, format: OutputFormat) -> io::Result<()> {
    if format == OutputFormat::Json {
        return super::write_json(
            out,
            &serde_json::to_value(snapshot).map_err(io::Error::other)?,
        );
    }
    let key = snapshot.key;
    writeln!(
        out,
        "Dispatcher: {} nsid={} ifindex={}",
        key.kind, key.nsid, key.ifindex
    )?;
    let mut fields = vec![("Revision", snapshot.revision.to_string())];
    match snapshot.runtime {
        Runtime::Xdp {
            program_id,
            kernel_link_id,
            ..
        } => {
            fields.push(("Program ID", program_id.to_string()));
            fields.push(("Kernel Link ID", kernel_link_id.to_string()));
        }
        Runtime::Tc {
            program_id,
            priority,
            filter_handle,
            ..
        } => {
            fields.push(("Program ID", program_id.to_string()));
            fields.push(("Priority", priority.to_string()));
            fields.push(("Filter Handle", format!("{filter_handle:#x}")));
        }
    }
    let width = fields.iter().map(|(key, _)| key.len()).max().unwrap_or(0) + 1;
    for (key, value) in fields {
        writeln!(out, "  {:width$} {value}", format!("{key}:"))?;
    }
    writeln!(
        out,
        "\nMembers ({}/{}):",
        snapshot.members.len(),
        bpfman_model::XDP_MAX_MEMBERS
    )?;
    let headers = [
        "POS",
        "PRIORITY",
        "PROGRAM ID",
        "FUNCTION NAME",
        "LINK ID",
        "KERNEL LINK ID",
        "PROCEED ON",
    ];
    let rows = snapshot
        .members
        .iter()
        .map(|m| {
            vec![
                m.position.to_string(),
                m.priority.to_string(),
                m.program_id.to_string(),
                m.program_name.into(),
                m.link_id.to_string(),
                m.kernel_link_id
                    .map_or_else(|| "<none>".into(), |id| id.to_string()),
                proceed_on(
                    m.proceed_on,
                    matches!(snapshot.runtime, Runtime::Xdp { .. }),
                ),
            ]
        })
        .collect::<Vec<_>>();
    table(out, "  ", &headers, &rows)
}

pub(super) fn table(
    out: &mut impl Write,
    indent: &str,
    headers: &[&str],
    rows: &[Vec<String>],
) -> io::Result<()> {
    let mut widths = headers
        .iter()
        .map(|h| h.chars().count())
        .collect::<Vec<_>>();
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    for row in std::iter::once(headers.iter().map(|h| (*h).to_owned()).collect::<Vec<_>>())
        .chain(rows.iter().cloned())
    {
        write!(out, "{indent}")?;
        for (index, cell) in row.iter().enumerate() {
            if index + 1 == row.len() {
                writeln!(out, "{cell}")?;
            } else {
                write!(out, "{cell:width$}  ", width = widths[index])?;
            }
        }
    }
    Ok(())
}

pub(super) fn proceed_on(mask: u32, xdp: bool) -> String {
    let actions: &[(u32, &str)] = if xdp {
        &[
            (0, "aborted"),
            (1, "drop"),
            (2, "pass"),
            (3, "tx"),
            (4, "redirect"),
            (31, "dispatcher_return"),
        ]
    } else {
        &[
            (0, "unspec"),
            (1, "ok"),
            (2, "reclassify"),
            (3, "shot"),
            (4, "pipe"),
            (5, "stolen"),
            (6, "queued"),
            (7, "repeat"),
            (8, "redirect"),
            (9, "trap"),
            (31, "dispatcher_return"),
        ]
    };
    let selected = actions
        .iter()
        .filter(|(bit, _)| mask & (1 << bit) != 0)
        .map(|(_, name)| *name)
        .collect::<Vec<_>>();
    if selected.is_empty() {
        "None".into()
    } else {
        selected.join(", ")
    }
}
