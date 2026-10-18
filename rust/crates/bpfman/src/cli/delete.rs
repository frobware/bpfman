//! Hidden cascading-delete input and reporting; runtime owns all mutations.
use bpfman_runtime::{DeleteOutcome, DeleteProgress, ProgramDelete};
use clap::Args;
use std::{
    fmt::Display,
    io::Write,
    num::{NonZeroU32, NonZeroU64},
};

#[derive(Args)]
pub(crate) struct DeletePrograms {
    /// Delete these programs, ordering selected map dependants before owners.
    #[arg(value_name = "PROGRAM_ID", required_unless_present = "all", conflicts_with = "all", num_args = 1..)]
    ids: Vec<NonZeroU32>,
    /// Delete every managed program.
    #[arg(long)]
    all: bool,
    /// Also delete unnamed programs sharing the target's maps.
    #[arg(short = 'r', long)]
    recursive: bool,
}

#[derive(Args)]
pub(crate) struct DeleteLinks {
    #[arg(value_name = "LINK_ID", required = true, num_args = 1..)]
    pub(super) ids: Vec<NonZeroU64>,
    /// Delete shared-map dependants of a program orphaned by the detach.
    #[arg(short = 'r', long)]
    pub(super) recursive: bool,
}

impl DeletePrograms {
    pub(crate) fn execute<S>(
        &self,
        app: &bpfman_runtime::Bpfman<S, bpfman_kernel_aya::Kernel>,
        cancellation: &bpfman_runtime::Cancellation,
    ) -> Result<(), crate::error::Error>
    where
        S: bpfman_store::UnloadStore
            + bpfman_store::LinkStore
            + bpfman_store::XdpReplacementStore
            + bpfman_store::TcStore
            + 'static,
        S::Reader: bpfman_store::LinkReader,
    {
        let selection = if self.all {
            ProgramDelete::All
        } else {
            ProgramDelete::Ids {
                ids: &self.ids,
                recursive: self.recursive,
            }
        };
        let results = app.delete_programs_with_cancellation(selection, cancellation)?;
        report("program", &results)
    }
}

pub(super) fn report<I: Display, S>(
    family: &str,
    results: &[DeleteOutcome<I, S, bpfman_kernel_aya::Kernel>],
) -> Result<(), crate::error::Error>
where
    S: bpfman_store::UnloadStore
        + bpfman_store::LinkStore
        + bpfman_store::XdpStore
        + bpfman_store::TcStore
        + 'static,
{
    let mut failed = 0;
    let mut cancelled = false;
    for result in results {
        for progress in &result.progress {
            if let DeleteProgress::Program { report, .. } = progress {
                for attempt in report.attempts() {
                    if let Err(error) = &attempt.outcome {
                        writeln!(
                            std::io::stderr().lock(),
                            "Warning: {:?}: {}",
                            attempt.kind,
                            crate::format_chain(error)
                        )?;
                    }
                }
            }
        }
        if let Err(error) = &result.outcome {
            failed += 1;
            cancelled |= error.kind() == bpfman_runtime::DeleteErrorKind::Cancelled;
            writeln!(
                std::io::stderr().lock(),
                "{family} {}: {}",
                result.id,
                crate::format_chain(error)
            )?;
        }
    }
    if failed != 0 {
        return Err(crate::error::Error::batch_delete(
            family,
            failed,
            results.len(),
            cancelled,
        ));
    }
    Ok(())
}
