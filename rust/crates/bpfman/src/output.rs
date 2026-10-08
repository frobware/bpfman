mod dispatcher;
pub(crate) use dispatcher::{dispatcher, dispatchers, tc_dispatcher};
use std::io::{self, Write};

use bpfman_model::StoredProgramSummary;

mod detail;
mod json;
mod link;

pub(super) use link::{link, links};

use crate::cli::OutputFormat;

pub(super) fn programs(
    out: &mut impl Write,
    programs: &[StoredProgramSummary],
    quiet: bool,
) -> io::Result<()> {
    if quiet {
        for program in programs {
            writeln!(out, "{}", program.id())?;
        }

        return Ok(());
    }

    table(out, programs)
}

fn table(out: &mut impl Write, programs: &[StoredProgramSummary]) -> io::Result<()> {
    if programs.is_empty() {
        return Ok(());
    }

    let with_application = programs.iter().any(|p| !p.application().is_empty());
    let mut headers = vec!["PROGRAM ID"];

    if with_application {
        headers.push("APPLICATION");
    }

    headers.extend(["TYPE", "FUNCTION NAME", "LINK IDS"]);
    let mut rows = vec![headers.into_iter().map(String::from).collect::<Vec<_>>()];

    for program in programs {
        let mut row = vec![program.id().to_string()];

        if with_application {
            row.push(program.application().into());
        }

        let links = if program.links().is_empty() {
            "<none>".into()
        } else {
            program
                .links()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        };
        row.extend([program.kind().to_string(), program.name().into(), links]);
        rows.push(row);
    }

    let mut widths = vec![0; rows[0].len()];

    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }

    for row in &rows {
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

pub(super) fn program(
    out: &mut impl Write,
    program: &bpfman_model::ObservedProgram,
    format: OutputFormat,
    loaded: bool,
) -> io::Result<()> {
    match format {
        OutputFormat::Text => detail::program(out, program),
        OutputFormat::Json => {
            let value = json::program(program);
            let value = if loaded {
                serde_json::json!({"programs":[value]})
            } else {
                value
            };
            write_json(out, &value)
        }
    }
}

pub(super) fn entries(
    out: &mut impl Write,
    entries: &[bpfman_model::ProgramEntry],
) -> io::Result<()> {
    write_json(
        out,
        &serde_json::json!({"programs":entries.iter().map(json::entry).collect::<Vec<_>>()}),
    )
}

fn write_json(out: &mut impl Write, value: &serde_json::Value) -> io::Result<()> {
    serde_json::to_writer_pretty(&mut *out, value).map_err(io::Error::other)?;
    writeln!(out)
}

/// Render one completed atomic load in input order.
pub(super) fn loaded_programs(
    out: &mut impl Write,
    programs: &[bpfman_model::ObservedProgram],
    format: OutputFormat,
) -> io::Result<()> {
    match format {
        OutputFormat::Text => {
            for program in programs {
                detail::program(out, program)?;
            }
            Ok(())
        }
        OutputFormat::Json => write_json(
            out,
            &serde_json::json!({"programs": programs.iter().map(json::program).collect::<Vec<_>>()}),
        ),
    }
}
