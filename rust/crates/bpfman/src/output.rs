use std::io::{self, Write};

use bpfman_model::StoredProgramSummary;

use crate::cli::OutputFormat;

pub(super) fn programs(
    out: &mut impl Write,
    programs: &[StoredProgramSummary],
    quiet: bool,
    format: OutputFormat,
) -> io::Result<()> {
    if quiet {
        for program in programs {
            writeln!(out, "{}", program.id())?;
        }
        return Ok(());
    }
    match format {
        OutputFormat::Text => table(out, programs),
    }
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
