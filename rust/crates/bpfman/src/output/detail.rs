//! Program detail presentation, shared by load and get.
use bpfman_model::{ObservedProgram, ProgramSource};
use std::io::{self, Write};
enum Row {
    Field(&'static str, String),
    Section(String, Vec<Row>),
    Note(&'static str),
}
fn field(label: &'static str, value: impl ToString) -> Row {
    Row::Field(label, value.to_string())
}
fn section(label: impl Into<String>, rows: Vec<Row>) -> Row {
    Row::Section(label.into(), rows)
}
fn render(out: &mut impl Write, rows: &[Row], depth: usize) -> io::Result<()> {
    let width = rows
        .iter()
        .filter_map(|r| {
            if let Row::Field(k, _) = r {
                Some(k.len())
            } else {
                None
            }
        })
        .max()
        .unwrap_or(0)
        + 1;
    let indent = "  ".repeat(depth);
    for row in rows {
        match row {
            Row::Field(k, v) => writeln!(
                out,
                "{}",
                format!("{indent}{:width$} {v}", format!("{k}:")).trim_end()
            )?,
            Row::Section(k, rows) => {
                writeln!(out, "{indent}{k}:")?;
                render(out, rows, depth + 1)?;
            }
            Row::Note(note) => writeln!(out, "{indent}{note}")?,
        }
    }
    Ok(())
}
fn sort(rows: &mut [Row]) {
    rows.sort_by(|a, b| match (a, b) {
        (Row::Field(a, _), Row::Field(b, _)) => a.cmp(b),
        _ => std::cmp::Ordering::Equal,
    });
}
pub(super) fn program(out: &mut impl Write, p: &ObservedProgram) -> io::Result<()> {
    let r = &p.record;
    let k = &p.kernel;
    let globals = if r.globals.is_empty() {
        "None".into()
    } else {
        r.globals
            .iter()
            .map(|(k, v)| {
                format!(
                    "{k}={}",
                    v.as_deref()
                        .unwrap_or_default()
                        .iter()
                        .map(|v| format!("{v:02x}"))
                        .collect::<String>()
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let metadata = if r.metadata.is_empty() {
        "None".into()
    } else {
        r.metadata
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut spec = vec![
        field("Global", globals),
        field("GPL Compatible", r.gpl_compatible),
        field(
            "License",
            if r.license.is_empty() {
                "None"
            } else {
                &r.license
            },
        ),
        field(
            "Map Owner ID",
            if r.map_set == r.id {
                "None".into()
            } else {
                r.map_set.to_string()
            },
        ),
        field("Map Pin Path", &r.map_path),
        field("Metadata", metadata),
        field("Name", r.spec.name().as_str()),
        field("Type", r.spec.kind()),
    ];
    match &r.source {
        ProgramSource::File(path) => spec.push(field("Path", path.as_deref().unwrap_or(""))),
        ProgramSource::Image {
            url, pull_policy, ..
        } => {
            spec.push(field("Image URL", url));
            spec.push(field("Pull Policy", super::json::policy(*pull_policy)));
        }
    };
    sort(&mut spec);
    let mut status = vec![
        field("Kernel Type", &k.kind),
        field("Bytecode", &p.bytecode),
        field("Instructions", k.verified_insns),
        field("Map Dir", &p.map_dir),
        field("Prog Pin", &p.prog_pin),
        field("Size JITted", format!("{} bytes", k.jited_size)),
        field(
            "Size Translated",
            if k.restricted {
                "(restricted)".into()
            } else {
                format!("{} bytes", k.xlated_size)
            },
        ),
        field("Tag", &k.tag),
    ];
    if let Some(id) = k.btf_id {
        status.push(field("BTF ID", id));
    }
    if let Some(time) = &k.loaded_at {
        status.push(field("Loaded At", time));
    }
    if let Some(mem) = k.memlock {
        status.push(field("Memory", format!("{mem} bytes")));
    }
    if !p.map_used_by.is_empty() {
        status.push(field(
            "Maps Used By",
            p.map_used_by
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" "),
        ));
    }
    sort(&mut status);
    status.push(field("Links", "None"));
    let maps = p
        .maps
        .iter()
        .map(|m| {
            let k = &m.kernel;
            let mut fields = vec![
                field("Key Size", format!("{}B", k.key_size)),
                field("Max Entries", k.max_entries),
                field("Name", &k.name),
            ];
            if let Some(pin) = &m.pin_path {
                fields.push(field(
                    "Pin",
                    format!("{pin}{}", if m.present { "" } else { " (missing)" }),
                ));
            }
            fields.extend([
                field("Type", &k.kind),
                field("Value Size", format!("{}B", k.value_size)),
            ]);
            section(k.id.to_string(), fields)
        })
        .collect::<Vec<_>>();
    status.push(if maps.is_empty() {
        field(
            "Maps",
            if let Some(ids) = k.map_ids.as_ref().filter(|ids| !ids.is_empty()) {
                format!(
                    "[{}]",
                    ids.iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            } else {
                "None".into()
            },
        )
    } else {
        section("Maps", maps)
    });
    let stats = match &p.stats {
        None => vec![Row::Note(
            "(not enabled, see sysctl kernel.bpf_stats_enabled)",
        )],
        Some(s) => {
            let mut rows = vec![
                field("Run Count", s.run_count),
                field("Runtime", duration(s.runtime_ns)),
            ];
            if s.recursion_misses > 0 {
                rows.push(field("Recursion Misses", s.recursion_misses));
            }
            sort(&mut rows);
            rows
        }
    };
    writeln!(out, "Program ID: {}", r.id)?;
    render(
        out,
        &[
            section("Spec", spec),
            section("Status", status),
            section("Stats", stats),
        ],
        1,
    )
}
fn scaled(value: u64, scale: u64, suffix: &str) -> String {
    let fraction = if value.is_multiple_of(scale) {
        String::new()
    } else {
        let width = scale.ilog10() as usize;
        format!(".{:0width$}", value % scale)
            .trim_end_matches('0')
            .to_owned()
    };
    format!("{}{fraction}{suffix}", value / scale)
}
fn duration(ns: u64) -> String {
    if ns == 0 {
        return "0s".into();
    }
    if ns < 1_000 {
        return format!("{ns}ns");
    }
    if ns < 1_000_000 {
        return scaled(ns, 1_000, "µs");
    }
    if ns < 1_000_000_000 {
        return scaled(ns, 1_000_000, "ms");
    }
    let seconds = ns / 1_000_000_000;
    let hours = seconds / 3600;
    let minutes = seconds % 3600 / 60;
    let mut text = String::new();
    if hours > 0 {
        text.push_str(&format!("{hours}h"));
    }
    if hours > 0 || minutes > 0 {
        text.push_str(&format!("{minutes}m"));
    }
    text.push_str(&scaled(ns % 60_000_000_000, 1_000_000_000, "s"));
    text
}

#[cfg(test)]
mod tests {
    #[test]
    fn duration_matches_go_units_and_trimming() {
        for (ns, text) in [
            (0, "0s"),
            (999, "999ns"),
            (1500, "1.5µs"),
            (1_250_000, "1.25ms"),
            (1_000_000_000, "1s"),
            (61_000_000_005, "1m1.000000005s"),
            (3_600_000_000_000, "1h0m0s"),
        ] {
            assert_eq!(super::duration(ns), text);
        }
    }
}
