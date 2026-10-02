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
        } => source.execute()?,
    }
    Ok(())
}
