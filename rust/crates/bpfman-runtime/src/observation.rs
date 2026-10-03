//! Full store/kernel views. Wire DTOs remain in the CLI.

use crate::{ActiveStore, Bpfman, ObservationError, ObservationErrorKind};
use bpfman_fs::{RuntimeDirectory, RuntimeLayout};
use bpfman_model::{
    KernelMap, KernelProgram, ObservedMap, ObservedProgram, ProgramEntry, ProgramStats,
    StoredProgram,
};
use bpfman_store::{OpenStore, ProgramReader};
use std::num::NonZeroU32;

#[derive(Debug, thiserror::Error)]
pub(super) enum Failure {
    #[error("program {0} does not exist")]
    Missing(NonZeroU32),
    #[error(
        "program {id} was recorded in the store snapshot but was absent during kernel observation"
    )]
    KernelMissing {
        id: NonZeroU32,
        #[source]
        cause: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("full observation of attached programs is not implemented")]
    Links,
    #[error("observe kernel object")]
    Kernel(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error(transparent)]
    Store(#[from] bpfman_store::Error),
    #[error(transparent)]
    Filesystem(#[from] bpfman_fs::Error),
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
            Failure::KernelMissing { .. } => ObservationErrorKind::KernelMissing,
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
                Failure::KernelMissing {
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

fn records<S: OpenStore>(store: &ActiveStore<S>) -> Result<Vec<StoredProgram>, Failure> {
    store.reader()?.read_records().map_err(Failure::from)
}

fn path(path: std::path::PathBuf) -> Result<String, Failure> {
    path.into_os_string()
        .into_string()
        .map_err(|_| Failure::Path)
}

impl<S: OpenStore> Bpfman<S> {
    /// Observe a managed record and its live kernel data without the writer lock.
    /// Kernel and pin observations may change after the store snapshot.
    /// Linked programs are rejected until full link observation is implemented.
    #[tracing::instrument(name = "program.get", level = "debug", skip_all, fields(program_id = id.get()), err)]
    pub fn get(&self, id: NonZeroU32) -> Result<ObservedProgram, ObservationError> {
        observe(&self.store, self.store.runtime(), id, View::Get)
    }
}

pub(super) fn observe<S: OpenStore>(
    store: &ActiveStore<S>,
    runtime: &RuntimeDirectory,
    id: NonZeroU32,
    view: View,
) -> Result<ObservedProgram, ObservationError> {
    let records = records(store)?;
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
        View::Get => runtime
            .read_map_pins(record.map_set)
            .map_err(Failure::from)?,
    };
    build(&mut Kernel, runtime.layout(), record, users, pins, view).map_err(ObservationError::from)
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

impl<S: OpenStore> Bpfman<S> {
    /// List full records and optional live kernel data without the writer lock.
    /// Missing kernel objects are null; other lookup failures remain errors.
    /// Use `list` for summaries without kernel privileges.
    #[tracing::instrument(name = "program.list_observed", level = "debug", skip_all, err)]
    pub fn list_entries(
        &self,
        filter: &bpfman_core::ProgramFilter,
    ) -> Result<Vec<ProgramEntry>, ObservationError> {
        let records = bpfman_core::select_records(records(&self.store)?, filter);

        entries(&mut Kernel, records).map_err(ObservationError::from)
    }
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
                Err(Failure::KernelMissing { .. }) => None,
                Err(error) => return Err(error),
            };

            Ok(ProgramEntry { record, kernel })
        })
        .collect()
}

#[cfg(test)]
mod tests;
