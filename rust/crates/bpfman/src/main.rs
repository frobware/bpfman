//! Composition root for the new Rust CLI.
//!
//! Clap owns argument diagnostics and help. Domain requests will be constructed
//! here as their corresponding runtime operations become available.

use std::{
    io::{self, Write},
    process::ExitCode,
};

use clap::{CommandFactory, FromArgMatches};

mod cli;
mod error;
mod ns_helper;
mod output;
mod signals;
mod telemetry;
mod version;

fn main() -> ExitCode {
    let definition = cli::Cli::command();
    let matches = definition.clone().get_matches();
    if let Err(error) = cli::validate_scopes(&definition, &matches) {
        error.exit();
    }
    let cli = cli::Cli::from_arg_matches(&matches).unwrap_or_else(|error| error.exit());
    if let Err(error) = cli.validate() {
        error.exit();
    }

    if let cli::Command::NsHelper { command } = cli.command {
        return match command.execute() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                let _ = writeln!(io::stderr().lock(), "Error: namespace helper: {error:#}");
                ExitCode::FAILURE
            }
        };
    }

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
    let _telemetry = telemetry::init(
        cli.trace_file.as_deref(),
        cli.log.as_deref(),
        cli.config.as_deref(),
    )?;
    if matches!(cli.command, cli::Command::Version) {
        version::write(&mut io::stdout().lock())?;
        return Ok(());
    }
    let _command =
        tracing::debug_span!("cli.command", runtime = %cli.layout.root().display()).entered();

    run_with_store(cli, bpfman_store_sqlite::Backend, cancellation)
}

fn run_with_store<S>(
    cli: cli::Cli,
    store: S,
    cancellation: &bpfman_runtime::Cancellation,
) -> Result<(), error::Error>
where
    S: bpfman_store::OpenStore
        + bpfman_store::DispatcherStore
        + bpfman_store::CommitLoad
        + bpfman_store::UnloadStore
        + bpfman_store::LinkStore
        + bpfman_store::XdpReplacementStore
        + bpfman_store::TcStore
        + 'static,
    S::Reader: bpfman_store::LinkReader + bpfman_store::XdpDispatcherReader,
{
    let command = cli.command.prepare(
        &cli.layout,
        cancellation,
        cli.config.as_deref(),
        &cli.image_cache,
        cli.lock_timeout,
        cli.trust_cache.as_ref(),
    )?;
    let command = match command {
        cli::PreparedCommand::Image(command) => {
            return command.execute(
                cancellation,
                cli.config.as_deref(),
                cli.trust_cache.as_ref(),
                cli.lock_timeout,
            );
        }
        other => other,
    };
    let store = bpfman_runtime::ActiveStore::open_with_cancellation(
        store,
        &cli.layout,
        cli.lock_timeout,
        cancellation,
    )?;
    let launcher = ns_helper::Launcher::new(std::env::current_exe()?, cancellation.clone());
    let kernel = bpfman_kernel_aya::Kernel::with_uprobe_target_opener(move |writer, target| {
        launcher.open(writer, target)
    });
    let bpfman = bpfman_runtime::Bpfman::new(store, kernel, cli.lock_timeout);

    match command {
        cli::PreparedCommand::Get { id, output } => {
            let program = bpfman.get_with_cancellation(id, cancellation)?;
            output::program(&mut io::stdout().lock(), &program, output, false)?;
        }

        cli::PreparedCommand::Unload {
            ids,
            ignore_missing,
        } => {
            // Zero is Go's absent-ID sentinel at the CLI boundary. Domain
            // requests remain nonzero, and even an all-zero batch takes the
            // ordinary cancellable writer scope before reporting its result.
            let selected: Vec<_> = ids
                .iter()
                .copied()
                .filter_map(std::num::NonZeroU32::new)
                .collect();
            let results = bpfman.unload_many_with_cancellation(&selected, cancellation)?;
            let mut outcomes = results.iter();
            let mut failed = 0;
            let mut cancelled = false;
            for &id in &ids {
                if id == 0 {
                    if !ignore_missing {
                        failed += 1;
                        writeln!(
                            io::stderr().lock(),
                            "program 0: managed program 0 does not exist"
                        )?;
                    }
                    continue;
                }
                let result = outcomes
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("incomplete program unload outcomes"))?;
                match &result.outcome {
                    Ok(report) => {
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
                    Err(error) if ignore_missing && error.is_program_missing() => {}
                    Err(error) => {
                        failed += 1;
                        cancelled |= error.kind() == bpfman_runtime::UnloadErrorKind::Cancelled;
                        writeln!(
                            io::stderr().lock(),
                            "program {}: {:#}",
                            result.id,
                            format_chain(error)
                        )?;
                    }
                }
            }
            if failed != 0 {
                return Err(error::Error::batch_unload(failed, ids.len(), cancelled));
            }
        }

        cli::PreparedCommand::Delete(args) => args.execute(&bpfman, cancellation)?,

        cli::PreparedCommand::List(args) => {
            let attachment = args.attachment();
            let filter = bpfman_core::ProgramFilter {
                types: args.types,
                application: args.application,
                metadata: args.metadata.into_iter().collect(),
                selector: args.selector,
            };

            if let Some(attachment) = attachment {
                let entries = bpfman.list_entries_by_attachment_with_cancellation(
                    &filter,
                    attachment,
                    cancellation,
                )?;
                output::all_entries(&mut io::stdout().lock(), &entries, args.output, args.quiet)?;
            } else if args.all {
                let entries = bpfman.list_all_entries_with_cancellation(&filter, cancellation)?;
                output::all_entries(&mut io::stdout().lock(), &entries, args.output, args.quiet)?;
            } else if args.output == cli::OutputFormat::Json && !args.quiet {
                let entries = bpfman.list_entries_with_cancellation(&filter, cancellation)?;
                output::entries(&mut io::stdout().lock(), &entries)?;
            } else {
                let programs = bpfman.list_with_cancellation(&filter, cancellation)?;
                output::programs(&mut io::stdout().lock(), &programs, args.quiet)?;
            }
        }

        cli::PreparedCommand::Image(command) => command.execute(
            cancellation,
            cli.config.as_deref(),
            cli.trust_cache.as_ref(),
            cli.lock_timeout,
        )?,
        cli::PreparedCommand::Load(request) => request.execute(&bpfman, cancellation)?,
        cli::PreparedCommand::Dispatcher(command) => command.execute(&bpfman, cancellation)?,
        cli::PreparedCommand::Link(command, mode) => {
            command.execute(&bpfman, cancellation, mode)?
        }
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
