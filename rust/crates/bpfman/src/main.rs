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
mod telemetry;

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
    let _telemetry = telemetry::init(cli.trace_file.as_deref())?;
    let _command =
        tracing::debug_span!("cli.command", runtime = %cli.layout.root().display()).entered();

    match cli.store {
        cli::StoreBackend::Sqlite => run_with_store(cli, bpfman_store_sqlite::Backend),
        cli::StoreBackend::Json => run_with_store(cli, bpfman_store_json::Backend),
    }
}

fn run_with_store<S>(cli: cli::Cli, store: S) -> anyhow::Result<()>
where
    S: bpfman_store::OpenStore + bpfman_store::CommitLoad + bpfman_store::UnloadStore + 'static,
{
    let command = cli.command.prepare(&cli.layout)?;
    let store = bpfman_runtime::ActiveStore::open(store, &cli.layout, cli.lock_timeout)?;
    let bpfman = bpfman_runtime::Bpfman::new(store, cli.lock_timeout);

    match command {
        cli::PreparedCommand::Get { id, output } => {
            let program = bpfman.get(id)?;
            output::program(&mut io::stdout().lock(), &program, output, false)?;
        }

        cli::PreparedCommand::Unload { id } => {
            let report = bpfman.unload(id)?;

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

        cli::PreparedCommand::List(args) => {
            let filter = bpfman_core::ProgramFilter {
                types: args.types,
                application: args.application,
            };

            if args.output == cli::OutputFormat::Json && !args.quiet {
                let entries = bpfman.list_entries(&filter)?;
                output::entries(&mut io::stdout().lock(), &entries)?;
            } else {
                let programs = bpfman.list(&filter)?;
                output::programs(&mut io::stdout().lock(), &programs, args.quiet)?;
            }
        }

        cli::PreparedCommand::Load(request) => request.execute(&bpfman)?,
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
