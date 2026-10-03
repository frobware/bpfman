//! Composition root for the new Rust CLI.
//!
//! Clap owns argument diagnostics and help. Domain requests will be constructed
//! here as their corresponding runtime operations become available.

use std::{
    io::{self, Write},
    process::ExitCode,
};

use clap::Parser;

mod cli;
mod output;

fn main() -> ExitCode {
    let cli = cli::Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "Error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: cli::Cli) -> anyhow::Result<()> {
    match cli.command {
        cli::Command::Program {
            command: cli::ProgramCommand::Unload { id },
        } => {
            let report = bpfman_runtime::unload_tracepoint(&cli.layout, id, cli.lock_timeout)?;
            for attempt in report.attempts() {
                if let Err(error) = &attempt.outcome {
                    writeln!(
                        io::stderr().lock(),
                        "Warning: {:?}: {:#}",
                        attempt.kind,
                        format_chain(error)
                    )?;
                }
            }
        }

        cli::Command::Program {
            command: cli::ProgramCommand::List(args),
        } => {
            let filter = bpfman_core::ProgramFilter {
                types: args.types,
                application: args.application,
            };
            let programs = bpfman_runtime::list_programs(&cli.layout, &filter, cli.lock_timeout)?;
            output::programs(&mut io::stdout().lock(), &programs, args.quiet, args.output)?;
        }
        cli::Command::Program {
            command: cli::ProgramCommand::Load { source },
        } => source.execute(&cli.layout, cli.lock_timeout)?,
    }
    Ok(())
}

fn format_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}
