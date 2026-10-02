use bpfman_core::{LoadFailure, LoadRollback, RollbackStep};
use bpfman_fs::RuntimeWriter;

use crate::LoadCleanup;

/// Execute one complete load-compensation pass under writer authority.
///
/// Every independent instruction is attempted exactly once in this pass, even
/// if all fail. No early error propagation or retry occurs inside the loop.
/// The returned report keeps the original cause and all cleanup observations;
/// only its unresolved instructions can be retried in a separate pass.
///
/// This synchronous driver cannot guarantee progress if an adapter hangs or
/// panics. Adapters must return failures normally and should not let cancelled
/// forward work prevent cleanup attempts. Process termination/crash recovery is
/// a separate concern. This is not a database rollback or an unload operation.
///
/// An unlocked directory is not sufficient authority:
///
/// ```compile_fail
/// use bpfman_core::LoadRollback;
/// use bpfman_fs::RuntimeDirectory;
/// use bpfman_runtime::{LoadCleanup, compensate_load};
/// fn unlocked<F: LoadCleanup>(directory: &RuntimeDirectory, fs: &mut F,
///     plan: LoadRollback<F::ProgramPin, F::MapPin, F::Bytecode, F::Error>) {
///     compensate_load(directory, fs, plan);
/// }
/// ```
pub fn compensate_load<F: LoadCleanup>(
    writer: &RuntimeWriter<'_>,
    filesystem: &mut F,
    mut rollback: LoadRollback<F::ProgramPin, F::MapPin, F::Bytecode, F::Error>,
) -> LoadFailure<F::ProgramPin, F::MapPin, F::Bytecode, F::Error> {
    loop {
        rollback = match rollback.next() {
            RollbackStep::RemoveBytecode { receipt, next } => {
                next.completed(filesystem.remove_bytecode(writer, receipt))
            }
            RollbackStep::RemoveProgramPin { receipt, next } => {
                next.completed(filesystem.remove_program_pin(writer, receipt))
            }
            RollbackStep::RemoveMapPin { receipt, next } => {
                next.completed(filesystem.remove_map_pin(writer, receipt))
            }
            RollbackStep::Complete(report) => return report,
        };
    }
}
