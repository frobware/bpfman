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
    let store = bpfman_store_sqlite::Backend;
    match cli.command {
        cli::Command::Program {
            command: cli::ProgramCommand::Get { id, output },
        } => {
            let program = bpfman_runtime::get_program(&store, &cli.layout, id, cli.lock_timeout)?;
            output::program(&mut io::stdout().lock(), &program, output, false)?;
        }

        cli::Command::Program {
            command: cli::ProgramCommand::Unload { id },
        } => {
            let report =
                bpfman_runtime::unload_tracepoint(&store, &cli.layout, id, cli.lock_timeout)?;
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
            if args.output == cli::OutputFormat::Json && !args.quiet {
                let entries = bpfman_runtime::list_program_entries(
                    &store,
                    &cli.layout,
                    &filter,
                    cli.lock_timeout,
                )?;
                output::entries(&mut io::stdout().lock(), &entries)?;
            } else {
                let programs =
                    bpfman_runtime::list_programs(&store, &cli.layout, &filter, cli.lock_timeout)?;
                output::programs(&mut io::stdout().lock(), &programs, args.quiet)?;
            }
        }
        cli::Command::Program {
            command: cli::ProgramCommand::Load { source },
        } => source.execute(&store, &cli.layout, cli.lock_timeout)?,
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
