use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum ImageCommand {
    /// Inspect public registry metadata without creating runtime state.
    Inspect {
        /// Bare, tagged or digest image reference.
        #[arg(value_name = "IMAGE", value_parser = reference)]
        reference: bpfman_image_oci::Reference,
    },
}

fn reference(value: &str) -> Result<bpfman_image_oci::Reference, String> {
    bpfman_image_oci::Reference::parse(value).map_err(|e| e.to_string())
}

impl ImageCommand {
    pub(crate) fn execute(
        self,
        cancellation: &bpfman_runtime::Cancellation,
    ) -> Result<(), crate::error::Error> {
        let Self::Inspect { reference } = self;
        let value = bpfman_image_oci::inspect(&reference, cancellation.flag())?;
        serde_json::to_writer_pretty(std::io::stdout().lock(), &value)
            .map_err(anyhow::Error::from)?;
        Ok(())
    }
}
