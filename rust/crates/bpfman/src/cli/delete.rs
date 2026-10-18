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
    #[arg(value_name = "PROGRAM_ID", num_args = 1.., value_parser = super::number::program_id)]
    ids: Vec<NonZeroU32>,
    /// Delete every managed program.
    #[arg(long, action = clap::ArgAction::Set, value_parser = super::number::boolean, num_args = 0..=1, require_equals = true, default_missing_value = "true", default_value_t = false)]
    all: bool,
    /// Also delete unnamed programs sharing the target's maps.
    #[arg(short = 'r', long, action = clap::ArgAction::Set, value_parser = super::number::boolean, num_args = 0..=1, require_equals = true, default_missing_value = "true", default_value_t = false)]
    recursive: bool,
}

#[derive(Args)]
pub(crate) struct DeleteLinks {
    #[arg(value_name = "LINK_ID", required = true, num_args = 1.., value_parser = super::number::link_id)]
    pub(super) ids: Vec<NonZeroU64>,
    /// Delete shared-map dependants of a program orphaned by the detach.
    #[arg(short = 'r', long, action = clap::ArgAction::Set, value_parser = super::number::boolean, num_args = 0..=1, require_equals = true, default_missing_value = "true", default_value_t = false)]
    pub(super) recursive: bool,
}

impl DeletePrograms {
    pub(super) fn validate(&self) -> Result<(), clap::Error> {
        if self.all && !self.ids.is_empty() {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "--all and explicit program IDs are mutually exclusive",
            ));
        }
        if !self.all && self.ids.is_empty() {
            return Err(clap::Error::raw(
                clap::error::ErrorKind::MissingRequiredArgument,
                "provide at least one program ID or use --all",
            ));
        }
        Ok(())
    }

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
