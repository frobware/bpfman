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
mod error;
mod output;
mod signals;
mod telemetry;

fn main() -> ExitCode {
    let cli = cli::Cli::parse();

    let shutdown = match signals::Shutdown::install() {
        Ok(shutdown) => shutdown,
        Err(error) => {
            let _ = writeln!(
                io::stderr().lock(),
                "Error: install signal handling: {error}"
            );
            return ExitCode::FAILURE;
        }
    };

    match run(cli, shutdown.cancellation()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "Error: {error:#}");
            shutdown.exit_code(&error)
        }
    }
}

fn run(cli: cli::Cli, cancellation: &bpfman_runtime::Cancellation) -> Result<(), error::Error> {
    let _telemetry = telemetry::init(cli.trace_file.as_deref())?;
    let _command =
        tracing::debug_span!("cli.command", runtime = %cli.layout.root().display()).entered();

    match cli.store {
        cli::StoreBackend::Sqlite => {
            run_with_store(cli, bpfman_store_sqlite::Backend, cancellation)
        }
        cli::StoreBackend::Json => run_with_store(cli, bpfman_store_json::Backend, cancellation),
    }
}

fn run_with_store<S>(
    cli: cli::Cli,
    store: S,
    cancellation: &bpfman_runtime::Cancellation,
) -> Result<(), error::Error>
where
    S: bpfman_store::OpenStore
        + bpfman_store::CommitLoad
        + bpfman_store::UnloadStore
        + bpfman_store::LinkStore
        + bpfman_store::XdpStore
        + 'static,
    S::Reader: bpfman_store::LinkReader + bpfman_store::XdpReader,
{
    let command = cli.command.prepare(&cli.layout, cancellation)?;
    let store = bpfman_runtime::ActiveStore::open_with_cancellation(
        store,
        &cli.layout,
        cli.lock_timeout,
        cancellation,
    )?;
    let bpfman = bpfman_runtime::Bpfman::new(store, bpfman_kernel_aya::Kernel, cli.lock_timeout);

    match command {
        cli::PreparedCommand::Get { id, output } => {
            let program = bpfman.get_with_cancellation(id, cancellation)?;
            output::program(&mut io::stdout().lock(), &program, output, false)?;
        }

        cli::PreparedCommand::Unload { id } => {
            let report = bpfman.unload_with_cancellation(id, cancellation)?;

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
                let entries = bpfman.list_entries_with_cancellation(&filter, cancellation)?;
                output::entries(&mut io::stdout().lock(), &entries)?;
            } else {
                let programs = bpfman.list_with_cancellation(&filter, cancellation)?;
                output::programs(&mut io::stdout().lock(), &programs, args.quiet)?;
            }
        }

        cli::PreparedCommand::Load(request) => request.execute(&bpfman, cancellation)?,
        cli::PreparedCommand::Dispatcher(command) => command.execute(&bpfman)?,
        cli::PreparedCommand::Link(command) => command.execute(&bpfman, cancellation)?,
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
