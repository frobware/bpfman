use clap::Subcommand;
use std::io::Write;

mod build;
mod verify;

#[derive(Subcommand)]
pub(crate) enum ImageCommand {
    /// Verify an image signature without creating runtime or cache state.
    Verify(verify::Verify),
    /// Build and push an OCI bytecode image without a container runtime.
    Build {
        /// Registry reference to publish.
        #[arg(value_name = "IMAGE", value_parser = reference)]
        reference: bpfman_image_oci::Reference,
        #[command(flatten)]
        registry_auth: super::registry_auth::RegistryAuth,
        #[command(flatten)]
        inputs: build::Inputs,
    },
    /// Print OCI build arguments and labels without publishing.
    GenerateBuildArgs {
        #[command(flatten)]
        inputs: build::Inputs,
        /// Output format.
        #[arg(short = 'o', long, value_enum, default_value = "text")]
        output: BuildOutput,
    },
    /// Inspect registry metadata without creating runtime state.
    Inspect {
        /// Bare, tagged or digest image reference.
        #[arg(value_name = "IMAGE", value_parser = reference)]
        reference: bpfman_image_oci::Reference,
        #[command(flatten)]
        registry_auth: super::registry_auth::RegistryAuth,
    },
}

#[derive(Clone, clap::ValueEnum)]
pub(crate) enum BuildOutput {
    Text,
    Json,
}

fn reference(value: &str) -> Result<bpfman_image_oci::Reference, String> {
    bpfman_image_oci::Reference::parse(value).map_err(|e| e.to_string())
}

impl ImageCommand {
    pub(crate) fn execute(
        self,
        cancellation: &bpfman_runtime::Cancellation,
        config: Option<&std::path::Path>,
    ) -> Result<(), crate::error::Error> {
        match self {
            Self::Verify(command) => command.execute(cancellation, config)?,
            Self::Inspect {
                reference,
                registry_auth,
            } => {
                let credentials = registry_auth
                    .credentials()
                    .unwrap_or_else(|error| error.exit());
                let value = bpfman_image_oci::inspect(
                    &reference,
                    credentials.as_ref(),
                    cancellation.flag(),
                )?;
                serde_json::to_writer_pretty(std::io::stdout().lock(), &value)
                    .map_err(anyhow::Error::from)?;
                writeln!(std::io::stdout().lock())?;
            }
            Self::GenerateBuildArgs { inputs, output } => {
                let plan = inputs.prepare(cancellation)?;
                std::io::stdout()
                    .lock()
                    .write_all(plan.render(matches!(output, BuildOutput::Json))?.as_bytes())?;
            }
            Self::Build {
                reference,
                inputs,
                registry_auth,
            } => {
                let credentials = registry_auth
                    .credentials()
                    .unwrap_or_else(|error| error.exit());
                let plan = inputs.prepare(cancellation)?;
                let raw = std::env::var("SOURCE_DATE_EPOCH").unwrap_or_default();
                let timestamp = if raw.trim().is_empty() {
                    0
                } else {
                    raw.trim()
                        .parse::<u64>()
                        .map_err(|e| anyhow::Error::new(e).context("invalid SOURCE_DATE_EPOCH"))?
                };
                let (publication, labels) = plan.publication()?;
                let pinned = bpfman_image_oci::publish(
                    &reference,
                    credentials.as_ref(),
                    publication,
                    &labels,
                    timestamp,
                    cancellation.flag(),
                )?;
                writeln!(std::io::stdout().lock(), "published: {pinned}")?;
            }
        }
        Ok(())
    }
}
