//! Full store/kernel views. Wire DTOs remain in the CLI.
use crate::{ObservationError, ObservationErrorKind};
use bpfman_fs::{RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use bpfman_lock::AcquireOptions;
use bpfman_model::{
    KernelMap, KernelProgram, ObservedMap, ObservedProgram, ProgramEntry, ProgramStats,
    StoredProgram,
};
use std::{num::NonZeroU32, time::Duration};
#[derive(Debug, thiserror::Error)]
pub(super) enum Failure {
    #[error("program {0} does not exist")]
    Missing(NonZeroU32),
    #[error("program {id} exists in store but not in kernel (requires reconciliation)")]
    Reconciliation {
        id: NonZeroU32,
        #[source]
        cause: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("full observation of attached programs is not implemented")]
    Links,
    #[error("observe kernel object")]
    Kernel(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error(transparent)]
    Store(#[from] bpfman_store_sqlite::Error),
    #[error(transparent)]
    Filesystem(#[from] bpfman_fs::Error),
    #[error("open program store")]
    Open(#[source] crate::Error),
    #[error("observed runtime path is not UTF-8")]
    Path,
}
impl From<bpfman_kernel::Error> for Failure {
    fn from(cause: bpfman_kernel::Error) -> Self {
        Self::Kernel(Box::new(cause))
    }
}
impl From<Failure> for ObservationError {
    fn from(cause: Failure) -> Self {
        Self { cause }
    }
}
impl ObservationError {
    /// Classify absence separately from denied or failed observation.
    pub fn kind(&self) -> ObservationErrorKind {
        match self.cause {
            Failure::Missing(_) => ObservationErrorKind::NotFound,
            Failure::Reconciliation { .. } => ObservationErrorKind::RequiresReconciliation,
            Failure::Links => ObservationErrorKind::Unsupported,
            _ => ObservationErrorKind::Unavailable,
        }
    }
}
#[derive(Clone, Copy)]
pub(super) enum View {
    Load,
    Get,
}
trait KernelObservations {
    fn program(&mut self, id: NonZeroU32)
    -> Result<(KernelProgram, Option<ProgramStats>), Failure>;
    fn map(&mut self, id: u32) -> Result<KernelMap, Failure>;
}
struct Kernel;
impl KernelObservations for Kernel {
    fn program(
        &mut self,
        id: NonZeroU32,
    ) -> Result<(KernelProgram, Option<ProgramStats>), Failure> {
        bpfman_kernel::observe_program(id).map_err(|cause| {
            if cause.kind() == bpfman_kernel::ErrorKind::Missing {
                Failure::Reconciliation {
                    id,
                    cause: Box::new(cause),
                }
            } else {
                cause.into()
            }
        })
    }
    fn map(&mut self, id: u32) -> Result<KernelMap, Failure> {
        bpfman_kernel::observe_map(id).map_err(Failure::from)
    }
}
fn records(writer: &RuntimeWriter<'_>) -> Result<Vec<StoredProgram>, Failure> {
    crate::store::open_store(writer)
        .map_err(Failure::Open)?
        .read_records()
        .map_err(Failure::from)
}
fn path(path: std::path::PathBuf) -> Result<String, Failure> {
    path.into_os_string()
        .into_string()
        .map_err(|_| Failure::Path)
}
/// Observe a managed program's record, live kernel data, maps and statistics.
/// Linked programs are rejected until full link observation is implemented.
pub fn get_program(
    layout: &RuntimeLayout,
    id: NonZeroU32,
    timeout: Duration,
) -> Result<ObservedProgram, ObservationError> {
    with_writer(layout, timeout, |writer| observe(writer, id, View::Get))
}
fn with_writer<T>(
    layout: &RuntimeLayout,
    timeout: Duration,
    run: impl FnOnce(&RuntimeWriter<'_>) -> Result<T, ObservationError>,
) -> Result<T, ObservationError> {
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).map_err(Failure::from)?;
    runtime
        .with_writer(
            AcquireOptions {
                timeout,
                cancelled: None,
            },
            |writer| run(&writer),
        )
        .map_err(Failure::from)?
}
pub(super) fn observe(
    writer: &RuntimeWriter<'_>,
    id: NonZeroU32,
    view: View,
) -> Result<ObservedProgram, ObservationError> {
    let records = records(writer)?;
    let index = records
        .iter()
        .position(|p| p.id == id)
        .ok_or(Failure::Missing(id))?;
    let record = records[index].clone();
    if !record.links.is_empty() {
        return Err(Failure::Links.into());
    }
    let users = records
        .iter()
        .filter(|p| p.map_set == record.map_set)
        .map(|p| p.id)
        .collect();
    let pins = match view {
        View::Load => Vec::new(),
        View::Get => writer
            .read_map_pins(record.map_set)
            .map_err(Failure::from)?,
    };
    build(&mut Kernel, writer.layout(), record, users, pins, view).map_err(ObservationError::from)
}
fn build<K: KernelObservations>(
    effects: &mut K,
    layout: &RuntimeLayout,
    record: StoredProgram,
    users: Vec<NonZeroU32>,
    pins: Vec<bpfman_fs::ObservedMapPin>,
    view: View,
) -> Result<ObservedProgram, Failure> {
    let (kernel, stats) = effects.program(record.id)?;
    let directory = layout.map_directory_path(record.map_set);
    let mut maps = Vec::new();
    for id in kernel.map_ids.as_deref().unwrap_or_default() {
        // Go omits an individually unreadable map; never synthesize attributes.
        let Ok(map) = effects.map(*id) else {
            continue;
        };
        let (pin_path, present) = match view {
            View::Load => (None, false),
            View::Get => match pins.iter().find(|pin| pin.id == map.id) {
                Some(pin) => (Some(path(directory.join(&pin.name))?), true),
                None if map.name.starts_with('.') => (None, false),
                None => (Some(path(directory.join(&map.name))?), false),
            },
        };
        maps.push(ObservedMap {
            kernel: map,
            pin_path,
            present,
        });
    }
    Ok(ObservedProgram {
        prog_pin: path(layout.program_pin_path(record.id))?,
        bytecode: path(layout.bytecode_path(record.id))?,
        map_dir: path(directory)?,
        record,
        kernel,
        stats: match view {
            View::Load => None,
            View::Get => stats,
        },
        maps,
        map_used_by: users,
    })
}
/// List full managed records and optional kernel observations. A missing kernel
/// object is null; permissions and other lookup failures remain errors. Existing
/// text/quiet listing stays available without kernel privileges.
pub fn list_program_entries(
    layout: &RuntimeLayout,
    filter: &bpfman_core::ProgramFilter,
    timeout: Duration,
) -> Result<Vec<ProgramEntry>, ObservationError> {
    with_writer(layout, timeout, |writer| {
        let records = bpfman_core::select_records(records(writer)?, filter);
        entries(&mut Kernel, records).map_err(ObservationError::from)
    })
}
fn entries<K: KernelObservations>(
    effects: &mut K,
    records: Vec<StoredProgram>,
) -> Result<Vec<ProgramEntry>, Failure> {
    records
        .into_iter()
        .map(|record| {
            let kernel = match effects.program(record.id) {
                Ok((kernel, _)) => Some(kernel),
                Err(Failure::Reconciliation { .. }) => None,
                Err(error) => return Err(error),
            };
            Ok(ProgramEntry { record, kernel })
        })
        .collect()
}
#[cfg(test)]
mod tests;
