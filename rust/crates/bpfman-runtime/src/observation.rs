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
    #[error("observation cancelled")]
    Cancelled,
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
    #[error(transparent)]
    Link(#[from] crate::LinkCause),
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
            Failure::Cancelled => ObservationErrorKind::Cancelled,
            Failure::Missing(_) => ObservationErrorKind::NotFound,
            Failure::KernelMissing { .. } => ObservationErrorKind::KernelMissing,
            Failure::Link(ref error) if error.kind() == crate::LinkErrorKind::Cancelled => {
                ObservationErrorKind::Cancelled
            }
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

struct Kernel<'a, K>(&'a K);

impl<K: bpfman_kernel::ProgramObservations> KernelObservations for Kernel<'_, K> {
    fn program(
        &mut self,
        id: NonZeroU32,
    ) -> Result<(KernelProgram, Option<ProgramStats>), Failure> {
        self.0.program(id).map_err(|cause| {
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
        self.0.map(id).map_err(Failure::from)
    }
}

fn records<S: OpenStore>(store: &ActiveStore<S>) -> Result<Vec<StoredProgram>, Failure> {
    store.reader().read_records().map_err(Failure::from)
}

fn path(path: std::path::PathBuf) -> Result<String, Failure> {
    path.into_os_string()
        .into_string()
        .map_err(|_| Failure::Path)
}

impl<S: OpenStore, K: bpfman_kernel::ProgramObservations + bpfman_kernel::LinkObservations>
    Bpfman<S, K>
where
    S::Reader: bpfman_store::LinkReader,
{
    /// Observe a managed record and its live kernel data without the writer lock.
    /// Kernel and pin observations may change after the store snapshot.
    pub fn get(&self, id: NonZeroU32) -> Result<ObservedProgram, ObservationError> {
        self.get_with_cancellation(id, &crate::Cancellation::new())
    }

    /// Observe a program with cancellation between snapshot and kernel reads.
    #[tracing::instrument(name = "program.get", level = "debug", skip_all, fields(program_id = id.get()), err)]
    pub fn get_with_cancellation(
        &self,
        id: NonZeroU32,
        cancellation: &crate::Cancellation,
    ) -> Result<ObservedProgram, ObservationError> {
        let mut observed = observe_cancellable(
            &self.kernel,
            &self.store,
            self.store.runtime(),
            id,
            View::Get,
            cancellation,
        )?;
        check(cancellation)?;
        let links = self
            .list_link_records_with_cancellation(cancellation)
            .map_err(Failure::from)?;

        // Store, kernel and pin observations are independent snapshots. Links
        // removed concurrently may be absent; never synthesize their presence.
        for record in links.into_iter().filter(|record| record.program_id == id) {
            observed.links.push(
                crate::link_observation::observe_record(
                    &self.kernel,
                    self.store.runtime(),
                    record,
                    cancellation,
                )
                .map_err(Failure::from)?,
            );
        }

        Ok(observed)
    }
}

pub(super) fn observe<S: OpenStore, K: bpfman_kernel::ProgramObservations>(
    kernel: &K,
    store: &ActiveStore<S>,
    runtime: &RuntimeDirectory,
    id: NonZeroU32,
    view: View,
) -> Result<ObservedProgram, ObservationError> {
    observe_cancellable(
        kernel,
        store,
        runtime,
        id,
        view,
        &crate::Cancellation::new(),
    )
}

fn check(cancellation: &crate::Cancellation) -> Result<(), Failure> {
    if cancellation.is_cancelled() {
        Err(Failure::Cancelled)
    } else {
        Ok(())
    }
}

fn observe_cancellable<S: OpenStore, K: bpfman_kernel::ProgramObservations>(
    kernel: &K,
    store: &ActiveStore<S>,
    runtime: &RuntimeDirectory,
    id: NonZeroU32,
    view: View,
    cancellation: &crate::Cancellation,
) -> Result<ObservedProgram, ObservationError> {
    check(cancellation)?;
    let records = records(store)?;
    check(cancellation)?;
    let index = records
        .iter()
        .position(|p| p.id == id)
        .ok_or(Failure::Missing(id))?;
    let record = records[index].clone();

    let users = records
        .iter()
        .filter(|p| p.map_set == record.map_set)
        .map(|p| p.id)
        .collect();
    let pins = match view {
        View::Load => Vec::new(),
        View::Get => kernel
            .map_pins(runtime, record.map_set)
            .map_err(Failure::from)?,
    };
    check(cancellation)?;
    build(
        &mut CancellableKernel(kernel, cancellation),
        runtime.layout(),
        record,
        users,
        pins,
        view,
    )
    .map_err(ObservationError::from)
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
        let map = match effects.map(*id) {
            Ok(map) => map,
            Err(Failure::Cancelled) => return Err(Failure::Cancelled),
            Err(_) => continue,
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
        links: Vec::new(),
    })
}

impl<S: OpenStore, K: bpfman_kernel::ProgramObservations> Bpfman<S, K> {
    /// List full records and optional live kernel data without the writer lock.
    /// Missing kernel objects are null; other lookup failures remain errors.
    /// Use `list` for summaries without kernel privileges.
    pub fn list_entries(
        &self,
        filter: &bpfman_core::ProgramFilter,
    ) -> Result<Vec<ProgramEntry>, ObservationError> {
        self.list_entries_with_cancellation(filter, &crate::Cancellation::new())
    }

    /// Observe entries with cancellation between individual kernel queries.
    #[tracing::instrument(name = "program.list_observed", level = "debug", skip_all, err)]
    pub fn list_entries_with_cancellation(
        &self,
        filter: &bpfman_core::ProgramFilter,
        cancellation: &crate::Cancellation,
    ) -> Result<Vec<ProgramEntry>, ObservationError> {
        check(cancellation)?;
        let records = bpfman_core::select_records(records(&self.store)?, filter);

        check(cancellation)?;
        entries(&mut CancellableKernel(&self.kernel, cancellation), records)
            .map_err(ObservationError::from)
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

struct CancellableKernel<'a, K>(&'a K, &'a crate::Cancellation);

impl<K: bpfman_kernel::ProgramObservations> KernelObservations for CancellableKernel<'_, K> {
    fn program(
        &mut self,
        id: NonZeroU32,
    ) -> Result<(KernelProgram, Option<ProgramStats>), Failure> {
        check(self.1)?;
        let result = Kernel(self.0).program(id)?;
        check(self.1)?;
        Ok(result)
    }

    fn map(&mut self, id: u32) -> Result<KernelMap, Failure> {
        check(self.1)?;
        let result = Kernel(self.0).map(id)?;
        check(self.1)?;
        Ok(result)
    }
}
