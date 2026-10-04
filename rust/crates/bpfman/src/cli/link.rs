use super::OutputFormat;
use bpfman_model::Tracepoint;
use clap::Subcommand;
use std::num::{NonZeroU32, NonZeroU64};

#[derive(Subcommand)]
pub(crate) enum LinkCommand {
    /// Attach an already loaded program.
    Attach {
        #[command(subcommand)]
        target: AttachCommand,
    },
    /// Detach one standalone link, keeping its program loaded.
    Detach {
        #[arg(value_name = "LINK_ID")]
        id: NonZeroU64,
    },
    /// Get stored intent and current kernel/pin observations.
    Get {
        #[arg(value_name = "LINK_ID")]
        id: NonZeroU64,
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    /// List stored standalone links, including pending intent, without a writer lock.
    List {
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
}

#[derive(Subcommand)]
pub(crate) enum AttachCommand {
    /// Attach a managed tracepoint program to GROUP/NAME.
    Tracepoint {
        #[arg(value_name = "PROGRAM_ID")]
        program_id: NonZeroU32,
        #[arg(value_name = "GROUP/NAME")]
        target: Tracepoint,
        #[arg(short = 'm', long, value_name = "KEY=VALUE", value_parser = metadata)]
        metadata: Vec<(String, String)>,
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
}

fn metadata(raw: &str) -> Result<(String, String), String> {
    let (key, value) = raw.split_once('=').ok_or("expected KEY=VALUE")?;
    let key = key.trim();
    if key.is_empty() {
        return Err("metadata key must not be empty".into());
    }

    Ok((key.into(), value.into()))
}

impl LinkCommand {
    pub(crate) fn execute<S>(
        self,
        app: &bpfman_runtime::Bpfman<S>,
        cancellation: &bpfman_runtime::Cancellation,
    ) -> Result<(), crate::error::Error>
    where
        S: bpfman_store::OpenStore + bpfman_store::LinkStore + 'static,
        S::Reader: bpfman_store::LinkReader,
    {
        match self {
            Self::Attach {
                target:
                    AttachCommand::Tracepoint {
                        program_id,
                        target,
                        metadata,
                        output,
                    },
            } => {
                let record = app.attach_tracepoint_with_cancellation(
                    bpfman_runtime::TracepointAttach {
                        program_id,
                        target,
                        metadata: metadata.into_iter().collect(),
                    },
                    cancellation,
                )?;
                // Finalisation has committed. Observation/output failure cannot
                // undo the link, and a late signal must not start compensation.
                let observed = app.get_link(record.id).map_err(|error| {
                    anyhow::Error::from(error).context(format!(
                        "link {} was committed but could not be observed; it remains attached",
                        record.id
                    ))
                })?;
                crate::output::link(&mut std::io::stdout().lock(), &observed, output)?;
            }
            Self::Detach { id } => {
                let _report = app.detach_with_cancellation(id, cancellation)?;
            }
            Self::Get { id, output } => {
                let link = app.get_link_with_cancellation(id, cancellation)?;
                crate::output::link(&mut std::io::stdout().lock(), &link, output)?;
            }
            Self::List { output } => {
                let links = app.list_link_records_with_cancellation(cancellation)?;
                crate::output::links(&mut std::io::stdout().lock(), &links, output)?;
            }
        }

        Ok(())
    }
}
